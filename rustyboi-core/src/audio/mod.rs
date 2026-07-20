pub mod controller;
mod envelope;
mod noise;
mod output;
mod square;
mod wave;

pub use controller::{Audio, ChannelSample, HOST_SAMPLE_RATE, NR52};
pub(crate) use controller::{
    NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24, NR30, NR31, NR32, NR33, NR34, NR41, NR42,
    NR43, NR44, NR50, NR51, WAV_END, WAV_START,
};
pub use output::AudioOutput;
