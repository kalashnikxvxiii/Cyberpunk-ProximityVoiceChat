//! Audio processing for the voice chat, independent of the game so it can be
//! tested on any platform. Everything runs at 48 kHz mono unless noted.

pub mod binaural;
pub mod filters;
pub mod jitter;
pub mod processor;
pub mod scene;
pub mod spatial;

pub use filters::{LowPass, Resampler, Reverb, Smooth, db_to_gain, rms_db};

pub const RATE: u32 = 48_000;
/// 20 ms, one Opus packet.
pub const FRAME: usize = 960;
/// 10 ms, what echo cancellation and RNNoise work on.
pub const HALF: usize = FRAME / 2;
