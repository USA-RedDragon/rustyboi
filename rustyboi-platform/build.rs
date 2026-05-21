// Emit a `mobile` cfg for the touch-oriented platforms (Android + iOS), so the
// mobile UI (on-screen controls, soft menu) can be gated with `#[cfg(mobile)]`
// instead of repeating `any(target_os = "android", target_os = "ios")`.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(mobile)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "android" || os == "ios" {
        println!("cargo::rustc-cfg=mobile");
    }
}
