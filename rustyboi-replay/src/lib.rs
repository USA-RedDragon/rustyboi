//! rustyboi-native recordings for the compat gallery: pixel-exact `.rbr` video
//! ([`video`]) and sample-exact `.rba` audio ([`audio`]), sharing one varint +
//! brotli stream layer ([`stream`]). ROM-free by construction — both formats
//! carry only rendered output. Encoding is host-side (`encode` feature); the
//! decoders compile to a small wasm module for client-side playback.
//!
//! Audio reconstruction goes through [`rustyboi_mix`], the emulator core's own
//! shared output stage — the BLEP step renderer, DAC-off fade, stereo mixer,
//! and output high-pass — so a decoded `.rba` is byte-equal to what the core
//! played, not an approximation of it. That crate is dependency-light (`libm`
//! only, `serde` non-default) precisely so this one can stay small enough to
//! ship in the gallery's wasm player — depending on the core itself would drag
//! `clap`, `zip`, `bincode`, and `serde` into the bundle.

#![forbid(unsafe_code)]

mod audio;
mod stream;
mod video;

/// Exact GB frame rate: 4194304 Hz / 70224 cycles-per-frame ≈ 59.7275 fps.
pub const FPS_NUM: u32 = 4_194_304;
pub const FPS_DEN: u32 = 70_224;

pub use audio::{AudioDecoder, AUDIO_RATE};
#[cfg(feature = "encode")]
pub use audio::AudioEncoder;
/// The per-sample audio contract and the machine's analog family — re-exported
/// from [`rustyboi_mix`] so encoder callers name one crate. The encoder's input
/// record IS `rustyboi_mix::SampleRecord`; there is no parallel replay tuple.
pub use rustyboi_mix::{AnalogModel, SampleRecord};
pub use stream::DecodeError;
#[cfg(feature = "encode")]
pub use video::encode;
#[cfg(feature = "encode")]
pub use video::Encoder;
pub use video::Decoder;
