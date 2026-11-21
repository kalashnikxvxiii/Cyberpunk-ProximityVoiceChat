//! What the game sounds like where the listener stands, read from the game's
//! own output: the background noise in the voice band, and what kind of noise
//! it is. (The game's reverb can't be read from it: idle sound is a steady
//! bed with no clean endings, recorded in eleven places. The reverb comes from
//! the raycasts.)

use std::collections::VecDeque;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::{RATE, Resampler};

/// 5 ms envelope blocks.
const BLOCK: usize = 240;
/// ~21 ms spectra.
const FFT: usize = 1024;
const HZ_PER_BIN: f32 = RATE as f32 / FFT as f32;
/// Spectral frames kept for the noise and its kind (3 s).
const FRAMES_KEPT: usize = 140;
/// Envelope blocks kept (3 s).
const BLOCKS_KEPT: usize = 600;
/// Below this a block is the game being silent, not a quiet place.
const SILENCE_DB: f32 = -90.0;
/// Local flatness is taken over groups of this many bins (750 Hz).
const GROUP: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Quiet,
    Music,
    Crowd,
    Traffic,
    /// Rain, wind, hiss: broad and steady.
    Rain,
    /// Shots, blows, crashes.
    Impacts,
    Mixed,
}

impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Music => "music",
            Self::Crowd => "crowd",
            Self::Traffic => "traffic",
            Self::Rain => "rain",
            Self::Impacts => "impacts",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Background in the voice band (300–4000 Hz), dBFS.
    pub noise_db: f32,
    pub kind: Kind,
}

#[derive(Clone, Copy)]
struct Bands {
    low: f32,
    speech: f32,
    high: f32,
    /// 0 = pure tones, ~0.56 = white noise.
    flat: f32,
}

pub struct Scene {
    resampler: Resampler,
    pcm: Vec<f32>,
    fft_in: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    norm: f32,
    spectrum: Vec<Complex<f32>>,
    frames: VecDeque<Bands>,
    blocks: VecDeque<f32>,
}

fn db(ms: f32) -> f32 {
    10.0 * (ms + 1e-12).log10()
}

fn percentile(values: impl Iterator<Item = f32>, p: f32) -> Option<f32> {
    let mut v: Vec<f32> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[((v.len() - 1) as f32 * p).round() as usize])
}

impl Scene {
    /// `rate`: the game's output rate.
    pub fn new(rate: u32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT);
        let window: Vec<f32> =
            (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
        // Parseval with the window: bin powers → mean square.
        let norm = 2.0 / (FFT as f32 * window.iter().map(|w| w * w).sum::<f32>());
        Self {
            resampler: Resampler::new(rate, RATE),
            pcm: Vec::new(),
            fft_in: Vec::with_capacity(FFT),
            spectrum: fft.make_output_vec(),
            fft,
            window,
            norm,
            frames: VecDeque::new(),
            blocks: VecDeque::new(),
        }
    }

    /// The game's sound, mono, at the rate given to [`Scene::new`].
    pub fn push(&mut self, input: &[f32]) {
        self.resampler.process(input, &mut self.pcm);
        let whole = self.pcm.len() / BLOCK * BLOCK;
        for i in (0..whole).step_by(BLOCK) {
            let block: [f32; BLOCK] = self.pcm[i..i + BLOCK].try_into().unwrap_or([0.0; BLOCK]);
            self.block(&block);
        }
        self.pcm.drain(..whole);
    }

    fn block(&mut self, b: &[f32; BLOCK]) {
        let level = db(b.iter().map(|x| x * x).sum::<f32>() / BLOCK as f32);
        // Digital silence (loading, a teleport, a cut scene's start) is no place: skip it.
        if level < SILENCE_DB {
            return;
        }
        self.blocks.push_back(level);
        if self.blocks.len() > BLOCKS_KEPT {
            self.blocks.pop_front();
        }
        for &x in b {
            self.fft_in.push(x);
            if self.fft_in.len() == FFT {
                self.spectrum_frame();
                self.fft_in.clear();
            }
        }
    }

    fn spectrum_frame(&mut self) {
        for (x, w) in self.fft_in.iter_mut().zip(&self.window) {
            *x *= w;
        }
        if self.fft.process(&mut self.fft_in, &mut self.spectrum).is_err() {
            return;
        }
        let power: Vec<f32> = self.spectrum.iter().map(|c| c.norm_sqr() * self.norm).collect();
        let bin = |hz: f32| ((hz / HZ_PER_BIN) as usize).min(power.len() - 1);
        let sum = |a: usize, b: usize| power[a..b].iter().sum::<f32>();
        let (s0, s1) = (bin(300.0), bin(4000.0));
        // Flatness per group, so a tilted spectrum (traffic, a far crowd) still reads as noise;
        // weighted by the groups' power, so the tones of music count, not their leakage.
        let flat = {
            let (mut weighted, mut total) = (0.0f32, 0.0f32);
            for g in power[s0..s1].chunks_exact(GROUP) {
                let mean = g.iter().sum::<f32>() / GROUP as f32;
                let geo = (g.iter().map(|p| (p + 1e-20).ln()).sum::<f32>() / GROUP as f32).exp();
                weighted += geo;
                total += mean;
            }
            if total > 1e-14 { weighted / total } else { 0.56 }
        };
        self.frames.push_back(Bands {
            low: db(sum(bin(20.0), bin(250.0))),
            speech: db(sum(s0, s1)),
            high: db(sum(s1, power.len())),
            flat,
        });
        if self.frames.len() > FRAMES_KEPT {
            self.frames.pop_front();
        }
    }

    /// `None` until a second of sound has been heard.
    pub fn reading(&self) -> Option<Reading> {
        if self.frames.len() < 40 {
            return None;
        }
        let f = &self.frames;
        let pct = |g: fn(&Bands) -> f32, p: f32| percentile(f.iter().map(g), p).unwrap_or(-120.0);
        let noise_db = pct(|b| b.speech, 0.3);
        let (low, speech, high, flat) = (pct(|b| b.low, 0.5), pct(|b| b.speech, 0.5), pct(|b| b.high, 0.5), pct(|b| b.flat, 0.5));
        // Steadiness over 0.2 s, so a beat counts but a single frame's chance doesn't.
        let lows: Vec<f32> = f
            .iter()
            .collect::<Vec<_>>()
            .chunks(10)
            .map(|c| db(c.iter().map(|b| 10f32.powf(b.low / 10.0)).sum::<f32>() / c.len() as f32))
            .collect();
        let low_spread = percentile(lows.iter().copied(), 0.9).unwrap_or(0.0) - percentile(lows.into_iter(), 0.1).unwrap_or(0.0);
        let blocks = || self.blocks.iter().copied();
        let loudest = percentile(blocks(), 0.99).unwrap_or(-120.0);
        let crest = loudest - percentile(blocks(), 0.5).unwrap_or(-120.0);
        let kind = if crest > 30.0 && loudest > -50.0 {
            Kind::Impacts
        } else if low.max(speech).max(high) < -55.0 {
            Kind::Quiet
        } else if low > speech + 8.0 && low_spread < 6.0 {
            Kind::Traffic
        } else if flat < 0.15 {
            Kind::Music
        } else if high >= speech && flat > 0.35 {
            Kind::Rain
        } else if speech >= low + 3.0 && speech >= high + 3.0 {
            Kind::Crowd
        } else {
            Kind::Mixed
        };
        Some(Reading { noise_db, kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LowPass;

    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Sum of three uniforms: close enough to Gaussian.
            let u = |s: u32| (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let a = u(self.0);
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = u(self.0);
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            a + b + u(self.0)
        }
    }

    fn scene_of(signal: &[f32]) -> Reading {
        let mut s = Scene::new(RATE);
        for chunk in signal.chunks(480) {
            s.push(chunk);
        }
        s.reading().expect("enough sound")
    }

    #[test]
    fn silence_of_a_loading_screen_is_not_a_quiet_place() {
        let mut n = Noise(7);
        let mut signal = vec![0.0; seconds(3.0)];
        signal.extend((0..seconds(2.0)).map(|_| 0.05 * n.next()));
        let r = scene_of(&signal);
        assert!(r.noise_db > -60.0, "{}", r.noise_db);
    }

    fn seconds(s: f32) -> usize {
        (s * RATE as f32) as usize
    }

    fn lowpassed(x: &mut [f32], hz: f32, times: usize) {
        for _ in 0..times {
            let mut f = LowPass::new();
            f.set_cutoff(hz);
            f.process(x);
        }
    }

    #[test]
    fn kinds() {
        let n = seconds(4.0);
        let mut rng = Noise(3);
        let music: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                [262.0, 330.0, 523.0, 784.0, 1047.0, 1568.0, 2093.0].iter().map(|f| 0.08 * (2.0 * std::f32::consts::PI * f * t).sin()).sum()
            })
            .collect();
        assert_eq!(scene_of(&music).kind, Kind::Music);

        let mut traffic: Vec<f32> = (0..n).map(|_| rng.next() * 0.5).collect();
        lowpassed(&mut traffic, 150.0, 2);
        assert_eq!(scene_of(&traffic).kind, Kind::Traffic);

        let rain: Vec<f32> = (0..n).map(|_| rng.next() * 0.1).collect();
        assert_eq!(scene_of(&rain).kind, Kind::Rain);

        // Babble: voice-band noise, swelling and falling four times a second.
        let raw: Vec<f32> = (0..n).map(|_| rng.next() * 0.3).collect();
        let mut lows = raw.clone();
        lowpassed(&mut lows, 300.0, 2);
        let mut crowd: Vec<f32> = raw.iter().zip(&lows).map(|(x, l)| x - l).collect();
        lowpassed(&mut crowd, 2500.0, 3);
        for (i, x) in crowd.iter_mut().enumerate() {
            *x *= 1.0 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * i as f32 / RATE as f32).sin();
        }
        assert_eq!(scene_of(&crowd).kind, Kind::Crowd);

        let impacts: Vec<f32> =
            (0..n).map(|i| if i % seconds(0.5) < seconds(0.01) { rng.next() * 1.5 } else { rng.next() * 1e-4 }).collect();
        assert_eq!(scene_of(&impacts).kind, Kind::Impacts);

        // A very quiet place (-70 dBFS), not the game's digital silence.
        assert_eq!(scene_of(&(0..n).map(|_| 0.0005 * rng.next()).collect::<Vec<_>>()).kind, Kind::Quiet);
    }

    #[test]
    fn voice_band_noise_level() {
        // White noise: the voice band holds 3700/24000 of its power.
        let mut rng = Noise(5);
        let x: Vec<f32> = (0..seconds(3.0)).map(|_| rng.next() * 0.2).collect();
        let ms = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
        let expected = db(ms * 3700.0 / 24_000.0);
        let got = scene_of(&x).noise_db;
        assert!((got - expected).abs() < 2.0, "{got} vs {expected}");
    }
}
