//! How voices travel through the game world, with Valve's Steam Audio: what
//! stands in the way (and what it's made of), how sound rebounds through
//! doors and down corridors, how places ring, and real 3D sound (HRTF).
//!
//! The geometry is the game's: Steam Audio asks for rays and a [`sa::Tracer`]
//! answers them (the plugin with the game's physics, the tests with boxes).

pub mod engine;
pub mod materials;
pub mod render;
pub mod sa;
pub mod world;

pub use engine::Engine;
pub use sa::Quality;

/// The default amount of simulation per run. Two bounces with many rays,
/// not three with few: the third adds little but grain (a 10 cm step could
/// change a voice through a doorway by 8 dB at 1024 rays x 3 bounces; at
/// 2048 x 2, under 1.5 dB: tests/scenes.rs). About 9000 game rays a run.
pub const QUALITY: Quality = Quality { rays: 2048, bounces: 2, occlusion_samples: 16, transmission_rays: 3 };

/// Where a voice rings down a tunnel: its rebounds must follow the bends.
/// At two bounces nothing got round a 90° bend (tests: a tunnel turning
/// twice, -141 dB just past the first); at four, -39 to -44 dB, as round a
/// tunnel's curve in game, where the voice was cut 30 m from its far exit,
/// one curve more (2026-11-21). Only there: a diner's doorway grows grainy.
/// (Not more rays: Steam Audio's rebounds grow with them, 4096 came 3 to 6
/// dB over 2048 down a tunnel, too loud in game, 2026-11-21.)
pub const RINGING: Quality = Quality { bounces: 4, ..QUALITY };
