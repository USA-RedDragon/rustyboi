mod gb;
mod cpu;
mod memory;

fn main() {
    let mut gb = gb::GB::new();
    gb.step();
}
