//! rustyboi-native recordings for the compat gallery: pixel-exact `.rbr` video
//! ([`video`]) and sample-exact `.rba` audio ([`audio`]), sharing one varint +
//! brotli stream layer ([`stream`]). ROM-free by construction — both formats
//! carry only rendered output. Encoding is host-side (`encode` feature); the
//! decoders compile to a small wasm module for client-side playback.

#![forbid(unsafe_code)]

mod audio;
mod stream;
mod video;

/// Exact GB frame rate: 4194304 Hz / 70224 cycles-per-frame ≈ 59.7275 fps.
pub const FPS_NUM: u32 = 4_194_304;
pub const FPS_DEN: u32 = 70_224;

pub use audio::{AudioDecoder, ChannelSample, AUDIO_RATE};
#[cfg(feature = "encode")]
pub use audio::AudioEncoder;
pub use stream::DecodeError;
#[cfg(feature = "encode")]
pub use video::encode;
#[cfg(feature = "encode")]
pub use video::Encoder;
pub use video::Decoder;
