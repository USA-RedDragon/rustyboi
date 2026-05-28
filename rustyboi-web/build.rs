// Enlarge the wasm shadow stack. A Game Boy savestate restore
// (`GB::from_state_bytes`, bincode) builds a large `GB` by value on the stack;
// wasm's default 1 MiB stack overflows during a state restore (rewind /
// load-state), trapping in the browser as "memory access out of bounds". 8 MiB
// clears it. Gated to wasm — native uses the OS stack — and applied to both the
// shipped cdylib and the wasm-bindgen integration tests so
// `rustyboi-web/tests/web_rewind.rs` exercises the same layout as production.
//
// This lives in build.rs (tracked) rather than `.cargo/config.toml` (gitignored
// in this repo), so the fix reaches every build and CI.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo::rustc-link-arg-cdylib=-zstack-size=8388608");
        println!("cargo::rustc-link-arg-tests=-zstack-size=8388608");
    }
}
