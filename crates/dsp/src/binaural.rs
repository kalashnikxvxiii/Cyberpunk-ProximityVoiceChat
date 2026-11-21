//! Our own 3D sound for headphones: a spherical head (Brown & Duda, "A
//! structural model for binaural sound synthesis", 1998), mono in, stereo out.
//!
//! - Each ear hears the sound when it gets there: up to 0.66 ms later on the
//!   far side (Woodworth's formula for a sphere).
//! - The head shadows the far ear and the near ear catches more of the highs:
//!   a one-pole/one-zero shelf per ear, from +6 dB facing the sound to -20 dB
//!   behind the head.
//! - The outer ears shadow sounds from behind: darker there, so front and
//!   back don't sound alike.
//! - The outer ear's folds echo the sound a few samples late, by how high it
//!   comes from: the notches they make rise with the source (Brown & Duda's
//!   pinna model), so above and below sound different.
//!
//! shortcut: a model, not a measured HRTF; upgrade with a public HRIR set if
//! players can't tell above from below where it matters.

use crate::filters::RATE;

/// Head radius (m) and speed of sound (m/s).
const HEAD_RADIUS: f32 = 0.0875;
const SOUND_SPEED: f32 = 343.0;
/// Delay line: more than the largest delay (31.5 samples between the ears, 20 more for the pinna) plus interpolation.
const DELAY_LEN: usize = 64;
/// Head shadow at its deepest (behind the head, from the ear's side).
const ALPHA_MIN: f32 = 0.1;
const THETA_MIN: f32 = 150.0;
/// The outer ears' shadow from straight behind: this much of the highs left, above about this.
const BACK_LEFT: f32 = 0.3;
const BACK_HZ: f32 = 2_500.0;
/// Brown & Duda's pinna echoes: (reflection, A, B, D), delays in samples at 44.1 kHz.
const PINNA: [(f32, f32, f32, f32); 5] =
    [(0.5, 1.0, 2.0, 1.0), (-1.0, 5.0, 4.0, 0.5), (0.5, 5.0, 7.0, 0.5), (-0.25, 5.0, 11.0, 0.5), (0.25, 5.0, 13.0, 0.5)];
/// How strong the echoes are against the model's (1): enough to place the voice, not to colour it.
const PINNA_MIX: f32 = 0.6;

/// Each pinna echo's delay (samples) for a sound from `dir`.
fn pinna_delays(dir: Dir) -> [f32; 5] {
    let lat = dir[0].clamp(-1.0, 1.0).asin();
    // Elevation around the ears' axis: 0 ahead, 90 above, 180 behind.
    let mut polar = dir[2].atan2(dir[1]).to_degrees();
    // Behind: the same cues as in front (the back shadow tells them apart).
    if polar > 90.0 {
        polar = 180.0 - polar;
    } else if polar < -90.0 {
        polar = -180.0 - polar;
    }
    PINNA.map(|(_, a, b, d)| (a * (lat / 2.0).cos() * (d * (90.0 - polar)).to_radians().sin() + b) * RATE / 44_100.0)
}

/// Where the sound comes from, relative to the head: x right, y front, z up (unit length).
pub type Dir = [f32; 3];

/// First-order shelf of Brown & Duda's head shadow for one ear.
#[derive(Clone, Copy, Default)]
struct Shadow {
    b0: f32,
    b1: f32,
    a1: f32,
    x1: f32,
    y1: f32,
}

impl Shadow {
    /// `cos_ear`: cosine of the angle between the ear's axis and the sound.
    fn set(&mut self, cos_ear: f32) {
        let theta = cos_ear.clamp(-1.0, 1.0).acos().to_degrees();
        let alpha = (1.0 + ALPHA_MIN / 2.0) + (1.0 - ALPHA_MIN / 2.0) * (theta / THETA_MIN * 180.0).to_radians().cos();
        // H(s) = (1 + alpha s / 2w0) / (1 + s / 2w0), w0 = c / a; bilinear transform.
        let tk = 2.0 * RATE / (2.0 * SOUND_SPEED / HEAD_RADIUS);
        let a0 = 1.0 + tk;
        (self.b0, self.b1, self.a1) = ((1.0 + alpha * tk) / a0, (1.0 - alpha * tk) / a0, (1.0 - tk) / a0);
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 - self.a1 * self.y1;
        (self.x1, self.y1) = (x, y);
        y
    }
}

pub struct Binaural {
    delay: [f32; DELAY_LEN],
    idx: usize,
    /// Each ear's delay (samples) as of the end of the last frame.
    late: [f32; 2],
    pinna: [f32; 5],
    shadow: [Shadow; 2],
    back_lp: f32,
    back: f32,
    gain: f32,
    started: bool,
}

impl Default for Binaural {
    fn default() -> Self {
        Self::new()
    }
}

impl Binaural {
    pub fn new() -> Self {
        Self {
            delay: [0.0; DELAY_LEN],
            idx: 0,
            late: [0.0; 2],
            pinna: [0.0; 5],
            shadow: [Shadow::default(); 2],
            back_lp: 0.0,
            back: 0.0,
            gain: 0.0,
            started: false,
        }
    }

    /// Adds `input` (mono) heard from `dir` at `gain` into interleaved stereo
    /// `out`. Gain, delays and the back shadow glide across the block.
    pub fn process(&mut self, input: &[f32], dir: Dir, gain: f32, out: &mut [f32]) {
        let right = dir[0].clamp(-1.0, 1.0);
        // Woodworth: the far ear is (a/c)(lat + sin lat) later.
        let lat = right.abs().asin();
        let itd = HEAD_RADIUS / SOUND_SPEED * (lat + lat.sin()) * RATE;
        let late = if right >= 0.0 { [itd, 0.0] } else { [0.0, itd] };
        self.shadow[0].set(-right);
        self.shadow[1].set(right);
        let back = (-dir[1]).max(0.0) * (1.0 - BACK_LEFT);
        let pinna = pinna_delays(dir);
        if !self.started {
            (self.late, self.pinna, self.back, self.gain, self.started) = (late, pinna, back, gain, true);
        }
        let n = input.len().max(1) as f32;
        let k = 1.0 - (-2.0 * std::f32::consts::PI * BACK_HZ / RATE).exp();
        for (i, (x, o)) in input.iter().zip(out.chunks_exact_mut(2)).enumerate() {
            let t = (i + 1) as f32 / n;
            // Behind: part of the highs taken away, the same for both ears.
            self.back_lp += (x - self.back_lp) * k;
            let b = self.back + (back - self.back) * t;
            let x = x + (self.back_lp - x) * b;
            self.delay[self.idx] = x;
            let g = self.gain + (gain - self.gain) * t;
            let tap = |d: f32| {
                let (whole, frac) = (d as usize, d.fract());
                let at = |back: usize| self.delay[(self.idx + DELAY_LEN - back) % DELAY_LEN];
                at(whole) + (at(whole + 1) - at(whole)) * frac
            };
            let echoes: [f32; 5] = std::array::from_fn(|k| self.pinna[k] + (pinna[k] - self.pinna[k]) * t);
            for ear in 0..2 {
                let d = self.late[ear] + (late[ear] - self.late[ear]) * t;
                let heard = PINNA.iter().zip(echoes).fold(tap(d), |sum, (&(r, ..), e)| sum + r * PINNA_MIX * tap(d + e));
                o[ear] += self.shadow[ear].process(heard) * g;
            }
            self.idx = (self.idx + 1) % DELAY_LEN;
        }
        (self.late, self.pinna, self.back, self.gain) = (late, pinna, back, gain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms_db;

    fn tone(hz: f32) -> Vec<f32> {
        (0..9600).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / RATE).sin() * 0.5).collect()
    }

    /// (left dB, right dB) of `hz` heard from `dir`, after it settles.
    fn ears(hz: f32, dir: Dir) -> (f32, f32) {
        let mut b = Binaural::new();
        let input = tone(hz);
        let mut out = vec![0.0; input.len() * 2];
        for (inp, o) in input.chunks(960).zip(out.chunks_mut(1920)) {
            b.process(inp, dir, 1.0, o);
        }
        let side = |c: usize| rms_db(&out[4800 * 2..].iter().skip(c).step_by(2).copied().collect::<Vec<_>>());
        (side(0), side(1))
    }

    #[test]
    fn front_is_even_and_sides_differ_in_the_highs() {
        let (l, r) = ears(4000.0, [0.0, 1.0, 0.0]);
        assert!((l - r).abs() < 0.1);
        let (l, r) = ears(4000.0, [1.0, 0.0, 0.0]);
        assert!(r - l > 12.0, "highs from the right: L {l:.1} R {r:.1}");
        // Lows bend around the head: barely any difference.
        let (l, r) = ears(150.0, [1.0, 0.0, 0.0]);
        assert!((r - l).abs() < 2.0, "lows from the right: L {l:.1} R {r:.1}");
    }

    #[test]
    fn far_ear_hears_it_later() {
        let mut b = Binaural::new();
        let mut input = vec![0.0f32; 960];
        input[100] = 1.0;
        let mut out = vec![0.0; 1920];
        b.process(&input, [-1.0, 0.0, 0.0], 1.0, &mut out);
        let peak = |c: usize| (0..960).max_by(|&i, &j| out[i * 2 + c].abs().total_cmp(&out[j * 2 + c].abs())).unwrap();
        let lag = peak(1) as i32 - peak(0) as i32;
        assert!((28..=34).contains(&lag), "right ear {lag} samples late");
    }

    #[test]
    fn above_and_below_sound_different() {
        let at = |hz: f32, z: f32| ears(hz, [0.0, (1.0 - z * z).sqrt(), z]).0;
        let differs = [5000.0, 6500.0, 8000.0, 10000.0].iter().map(|&hz| (at(hz, 0.9) - at(hz, -0.9)).abs()).fold(0.0, f32::max);
        assert!(differs > 6.0, "above vs below: {differs:.1} dB at most");
        // Ahead, speech's main band keeps its tone.
        for hz in [200.0, 500.0, 1000.0, 2000.0] {
            let front = at(hz, 0.0);
            assert!((front - rms_db(&tone(hz))).abs() < 4.5, "{hz} Hz ahead: {front:.1}");
        }
    }

    #[test]
    fn behind_is_darker_than_in_front() {
        let front = ears(6000.0, [0.0, 1.0, 0.0]).0;
        let back = ears(6000.0, [0.0, -1.0, 0.0]).0;
        assert!(front - back > 5.0, "front {front:.1} back {back:.1}");
        // Speech's body is left alone.
        assert!((ears(300.0, [0.0, 1.0, 0.0]).0 - ears(300.0, [0.0, -1.0, 0.0]).0).abs() < 1.5);
    }
}
