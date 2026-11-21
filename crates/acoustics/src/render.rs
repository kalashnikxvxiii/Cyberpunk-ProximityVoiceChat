//! Turning a voice into what our ears get, on the audio thread: the direct
//! sound (muffled by what's in the way, placed with the HRTF) plus its
//! rebounds (convolved with the simulated room, from every side they come).

use crate::engine::Engine;
use crate::sa::{AMBI_CHANNELS, BinauralEffect, Buffer, DecodeEffect, Direct, DirectEffect, FRAME, ReflectionEffect, Source, Space, V3};

/// Steam Audio's HRTF 3 dB under the binaural model the mod had (in
/// energy, tests/level.rs): its ears' resonances make a voice sound louder
/// than it measures; at equal energy it came out too loud (in game, 2026-11-20).
pub const HRTF_LEVEL: f32 = 0.62;

/// One way a voice reaches us straight: muffled, then placed with the HRTF.
struct Way {
    direct: DirectEffect,
    binaural: BinauralEffect,
    /// The same voice from the mirrored side (see [`Way::place`]).
    mirrored: BinauralEffect,
}

/// Buffers the ways share, one after the other.
struct Scratch {
    muffled: Buffer,
    stereo: Buffer,
    stereo_m: Buffer,
}

impl Way {
    fn new(e: &Engine) -> Result<Self, String> {
        Ok(Self { direct: DirectEffect::new(&e.ctx)?, binaural: BinauralEffect::new(&e.ctx, &e.hrtf)?, mirrored: BinauralEffect::new(&e.ctx, &e.hrtf)? })
    }

    /// Adds `mono` through `d` from `dir` into `mix` (interleaved stereo).
    #[allow(clippy::too_many_arguments)]
    fn place(&mut self, e: &Engine, d: &Direct, gain: f32, air: [f32; 3], dir: V3, mono: &mut Buffer, b: &mut Scratch, mix: &mut [f32]) {
        self.direct.apply(d, gain * HRTF_LEVEL, air, mono, &mut b.muffled);
        // Steam Audio's HRTF is one real head, lopsided: a voice straight
        // ahead comes 2 dB louder in the right ear, one 15° right sounds like
        // 25°, one 15° left like 8° (tests/level.rs). Both ears from its left
        // one: the right ear hears what the left would with the voice
        // mirrored. A symmetric head, still telling front from back, up from
        // down. (Averaging both ears instead rang metallic: their delays differ.)
        let mirror = V3 { x: -dir.x, ..dir };
        self.binaural.apply(&e.hrtf, dir, &mut b.muffled, &mut b.stereo);
        self.mirrored.apply(&e.hrtf, mirror, &mut b.muffled, &mut b.stereo_m);
        for (i, lr) in mix.chunks_exact_mut(2).take(FRAME).enumerate() {
            lr[0] += b.stereo.channel(0)[i];
            lr[1] += b.stereo_m.channel(0)[i];
        }
    }
}

/// One speaker's effects and buffers.
pub struct Voice {
    /// Straight from them, through what's in the way.
    straight: Way,
    /// Round the edges of openings (see [`Heard::bends`]).
    bent: [Way; 2],
    rebounds: ReflectionEffect,
    mono: Buffer,
    scratch: Scratch,
    ambi: Buffer,
    /// The voice's energy and its rebounds' (W), averaged over the frames
    /// it talks: their ratio, with the direct part, is how loud it reaches us.
    /// (A ratio of averages: a frame-by-frame ratio shot up between words,
    /// the rebounds still ringing over a silent input, 2026-11-20.)
    talk_energy: f32,
    rebound_energy: f32,
    /// Their level (dB) followed over seconds (see [`Voice::spike`]).
    slow_db: Option<f32>,
    /// The simulation gave rebounds for this voice (diagnostics).
    pub has_rebounds: bool,
    /// Where the rebounds come from (W times X, Y, Z, averaged with the
    /// energies): the doorway they pour out of, the far end of a tunnel.
    arrival: [f32; 3],
    /// What goes into the rebounds (see [`Shelf`]).
    shelf: Shelf,
}

/// Steam Audio's rebounds, decoded to our ears, came 9 to 12 dB heavier
/// under 500 Hz than the direct sound (tests/scenes.rs, octave by octave):
/// a room's ring booming over the voice, louder the farther away (in game,
/// 2026-11-20). Concrete absorbs almost evenly; the boom is the decoding's.
/// A low shelf on what goes in takes it out; the gain keeps a voice's
/// rebounds out of sight about as loud as before, the bass aside.
const SHELF_DB: f32 = -11.0;
const SHELF_HZ: f32 = 600.0;
const SHELF_GAIN: f32 = 2.4;

/// Rebounds reach our ears this much louder against the direct sound than
/// [`Voice::rebound_level`] says (decoding, and the direct's [`HRTF_LEVEL`]):
/// +10 dB, measured with speech in a diner and a room (tests/scenes.rs).
pub const EAR: f32 = 3.1;

/// RBJ low shelf (slope 1), at 48 kHz.
struct Shelf {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
    /// Share of the way to flat (see [`Voice::set_ring`]).
    off: f32,
}

impl Shelf {
    fn new() -> Self {
        Self::with(0.0, [0.0; 2])
    }

    /// `off` of the way to flat (gain 1, no shelf), filter state `z`.
    fn with(off: f32, z: [f32; 2]) -> Self {
        let (db, gain) = (SHELF_DB * (1.0 - off), SHELF_GAIN + (1.0 - SHELF_GAIN) * off);
        let a = 10f32.powf(db / 40.0);
        let w = 2.0 * std::f32::consts::PI * SHELF_HZ / 48_000.0;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
        let k = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cos + k;
        let g = gain * a / a0;
        Self {
            b: [g * ((a + 1.0) - (a - 1.0) * cos + k), g * 2.0 * ((a - 1.0) - (a + 1.0) * cos), g * ((a + 1.0) - (a - 1.0) * cos - k)],
            a: [-2.0 * ((a - 1.0) + (a + 1.0) * cos) / a0, ((a + 1.0) + (a - 1.0) * cos - k) / a0],
            z,
            off,
        }
    }

    fn process(&mut self, x: &mut [f32]) {
        for v in x {
            let y = self.b[0] * *v + self.z[0];
            self.z[0] = self.b[1] * *v - self.a[0] * y + self.z[1];
            self.z[1] = self.b[2] * *v - self.a[1] * y;
            *v = y;
        }
    }
}

/// Averaging per 20 ms frame for [`Voice::spike`]: about 1.5 s.
const SLOW: f32 = 0.013;
/// How far over their recent level rebounds may jump at once (dB).
const SPIKE_DB: f32 = 6.0;
/// Over their recent level by this much (dB), they come from a new place.
const NEW_PLACE_DB: f32 = 20.0;

/// Frames quieter than this (-70 dBFS) are pauses: not averaged.
const TALKING: f32 = 1e-7;
/// Averaging per 20 ms frame: about a quarter of a second (slower lagged
/// behind walking out of a doorway, in game 2026-11-20).
const AVERAGE: f32 = 0.08;

/// What a voice sounds like this frame.
#[derive(Debug, Clone, Copy)]
pub struct Heard {
    /// Our distance law, the shout's reach, the volume.
    pub gain: f32,
    pub direct: Direct,
    /// Where the source is, the listener's axes (HRTF input).
    pub dir: V3,
    /// Highs the air absorbs over the distance (see [`air`]).
    pub air: [f32; 3],
    /// Gain into the rebounds (0: none).
    pub rebounds: f32,
    /// What bends round the edges of openings (doorways, corners), each
    /// heard from its own: a voice comes out of every way it has at once.
    pub bends: [Option<Bend>; 2],
}

/// A way round an opening's edge.
#[derive(Debug, Clone, Copy)]
pub struct Bend {
    /// Against [`Heard::gain`].
    pub gain: f32,
    /// Low/mid/high: the sharper the turn, the darker.
    pub eq: [f32; 3],
    pub dir: V3,
    pub air: [f32; 3],
}

fn energy(x: &[f32]) -> f32 {
    x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32
}

impl Voice {
    pub fn new(e: &Engine) -> Result<Self, String> {
        Ok(Self {
            straight: Way::new(e)?,
            bent: [Way::new(e)?, Way::new(e)?],
            rebounds: ReflectionEffect::new(&e.ctx)?,
            mono: Buffer::new(1),
            scratch: Scratch { muffled: Buffer::new(1), stereo: Buffer::new(2), stereo_m: Buffer::new(2) },
            ambi: Buffer::new(AMBI_CHANNELS),
            talk_energy: 0.0,
            rebound_energy: 0.0,
            slow_db: None,
            has_rebounds: false,
            arrival: [0.0; 3],
            shelf: Shelf::new(),
        })
    }

    /// Adds `input` (mono, one frame) heard as `h` into `mix` (interleaved
    /// stereo) and its rebounds into `ambi_mix`. `source`: None while the
    /// simulation hasn't seen this voice yet (no rebounds).
    pub fn render(&mut self, e: &Engine, source: Option<&Source>, h: &Heard, input: &[f32], mix: &mut [f32], ambi_mix: &mut Buffer) {
        self.mono.channel_mut(0)[..input.len().min(FRAME)].copy_from_slice(&input[..input.len().min(FRAME)]);
        self.straight.place(e, &h.direct, h.gain, h.air, h.dir, &mut self.mono, &mut self.scratch, mix);
        for (way, b) in self.bent.iter_mut().zip(&h.bends) {
            if let Some(b) = b.filter(|b| b.gain > 0.0) {
                let d = Direct { occlusion: 0.0, transmission: b.eq };
                way.place(e, &d, h.gain * b.gain, b.air, b.dir, &mut self.mono, &mut self.scratch, mix);
            }
        }
        let input_energy = energy(self.mono.channel(0));
        self.shelf.process(self.mono.channel_mut(0));
        match source {
            // Run even when not heard (gain 0): the level is measured here,
            // and a voice gone silent must be able to come back.
            Some(s) => {
                self.has_rebounds = self.rebounds.apply(s, &mut self.mono, &mut self.ambi);
                if input_energy > TALKING {
                    self.talk_energy += (input_energy - self.talk_energy) * AVERAGE;
                    // W (channel 0) carries the rebounds' level from all around.
                    self.rebound_energy += (energy(self.ambi.channel(0)) - self.rebound_energy) * AVERAGE;
                    let now = 20.0 * self.rebound_level().max(1e-6).log10();
                    self.slow_db = Some(match self.slow_db {
                        // Far over: another place (back from outside a tunnel's mouth), not a spike.
                        Some(s) if now > s + NEW_PLACE_DB => now - SPIKE_DB,
                        Some(s) => s + (now - s) * SLOW,
                        None => now,
                    });
                    let w = self.ambi.channel(0);
                    // Steam Audio's channels 1, 2, 3 point left, up, ahead (-x, y, -z
                    // of its axes), whatever way the listener faces (tests/scenes.rs).
                    for (k, (ch, sign)) in [(1, -1.0), (2, 1.0), (3, -1.0)].into_iter().enumerate() {
                        let along = sign * w.iter().zip(self.ambi.channel(ch)).map(|(a, b)| a * b).sum::<f32>() / w.len() as f32;
                        self.arrival[k] += (along - self.arrival[k]) * AVERAGE;
                    }
                }
                if h.rebounds > 0.0 {
                    ambi_mix.add(&self.ambi, h.rebounds);
                }
            }
            None => {
                self.has_rebounds = false;
                self.rebound_energy *= 0.95;
            }
        }
    }

    /// How much the place rings all round (0 to 1): a tunnel's rebounds go
    /// in unshelved, their bass and all, as they were before the diner's
    /// boom was taken out (they carried the voice down a tunnel just right,
    /// in game 2026-11-20).
    pub fn set_ring(&mut self, ring: f32) {
        let ring = ring.clamp(0.0, 1.0);
        let at_end = ring == 0.0 || ring == 1.0;
        if (ring - self.shelf.off).abs() > 0.01 || (at_end && ring != self.shelf.off) {
            self.shelf = Shelf::with(ring, self.shelf.z);
        }
    }

    /// What takes the simulation's spikes out (amplitude, at most 1): each
    /// run's rays find a little more or less, and down a tunnel the rebounds
    /// jumped up by as much as 14 dB from one run to the next (in game
    /// 2026-11-21). Over [`SPIKE_DB`] above their level of the last seconds,
    /// they're held there; never raised (evening them out both ways, averages
    /// of ratios, made a tunnel's voice far too loud).
    pub fn spike(&self) -> f32 {
        let now = 20.0 * self.rebound_level().max(1e-6).log10();
        self.slow_db.map_or(1.0, |s| 10f32.powf(((s + SPIKE_DB - now) / 20.0).min(0.0)))
    }

    /// Which way (world, Steam Audio's axes, unit) the rebounds come from,
    /// once there are any.
    pub fn arrival(&self) -> Option<V3> {
        let [x, y, z] = self.arrival;
        let l = (x * x + y * y + z * z).sqrt();
        (l > 1e-12).then(|| V3 { x: x / l, y: y / l, z: z / l })
    }

    /// The rebounds' level against the voice (amplitude), before their gain.
    pub fn rebound_level(&self) -> f32 {
        if self.talk_energy > 0.0 { (self.rebound_energy / self.talk_energy).sqrt() } else { 0.0 }
    }
}

/// All voices' rebounds, turned to our head once per frame.
pub struct Decoder {
    effect: DecodeEffect,
    pub ambi: Buffer,
    out: Buffer,
}

impl Decoder {
    pub fn new(e: &Engine) -> Result<Self, String> {
        Ok(Self { effect: DecodeEffect::new(&e.ctx, &e.hrtf)?, ambi: Buffer::new(AMBI_CHANNELS), out: Buffer::new(2) })
    }

    /// Adds the frame's rebounds into `mix` (interleaved stereo) and clears them.
    pub fn decode(&mut self, e: &Engine, listener: &Space, mix: &mut [f32]) {
        self.effect.apply(&e.hrtf, listener, &mut self.ambi, &mut self.out);
        for (i, lr) in mix.chunks_exact_mut(2).take(FRAME).enumerate() {
            lr[0] += self.out.channel(0)[i];
            lr[1] += self.out.channel(1)[i];
        }
        self.ambi.clear();
    }
}

/// Air over `distance` metres, low/mid/high: Steam Audio's default
/// coefficients (a far shout comes thin).
pub fn air(distance: f32) -> [f32; 3] {
    [0.0002f32, 0.0017, 0.0182].map(|k| (-k * distance.max(0.0)).exp())
}

/// Direction of `source` from `listener`, the listener's axes.
pub fn direction(e: &Engine, source: V3, listener: &Space) -> V3 {
    e.ctx.relative(source, listener)
}


#[cfg(test)]
mod tests {
    use super::Shelf;

    #[test]
    fn shelf_fades_to_flat() {
        let mut flat = Shelf::with(1.0, [0.0; 2]);
        let mut x: Vec<f32> = (0..480).map(|i| (i as f32 * 0.37).sin()).collect();
        let y = x.clone();
        flat.process(&mut x);
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-5));
        // Fully on: bass (50 Hz) cut against 5 kHz, as SHELF_DB says.
        let level = |hz: f32| {
            let mut s = Shelf::new();
            let mut x: Vec<f32> = (0..48_000).map(|i| (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin()).collect();
            s.process(&mut x);
            (x[24_000..].iter().map(|v| v * v).sum::<f32>() / 24_000.0 * 2.0).sqrt()
        };
        let tilt = 20.0 * (level(50.0) / level(5000.0)).log10();
        assert!((tilt - super::SHELF_DB).abs() < 1.0, "{tilt}");
    }
}
