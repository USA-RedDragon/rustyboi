mod analog;
pub mod controller;
mod envelope;
mod length;
mod noise;
mod output;
mod square;
mod wave;

pub(crate) use analog::AnalogModel;
pub use controller::{Audio, ChannelSample, HOST_SAMPLE_RATE, NR52};

/// The stereo mixer and DAC transfer function, which this crate shares verbatim
/// with the `.rba` replay decoder — see [`rustyboi_mix`] for why they live in a
/// dependency-free leaf crate rather than here. Re-exported so a consumer that
/// already depends on the core can reach the mixer without naming a second
/// crate.
pub use rustyboi_mix as mix;
pub(crate) use controller::{
    NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24, NR30, NR31, NR32, NR33, NR34, NR41, NR42,
    NR43, NR44, NR50, NR51, WAV_END, WAV_START,
};
pub use output::AudioOutput;
