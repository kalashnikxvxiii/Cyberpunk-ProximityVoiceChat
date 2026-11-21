//! Small signal-processing pieces used by the audio engine. All at 48 kHz.

pub const RATE: f32 = crate::RATE as f32;

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// RMS level in dBFS; -100 for silence.
pub fn rms_db(x: &[f32]) -> f32 {
    if x.is_empty() {
        return -100.0;
    }
    let ms = x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32;
    if ms <= 1e-10 { -100.0 } else { 10.0 * ms.log10() }
}

/// Streaming linear resampler; good enough for voice.
pub struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, pos: 0.0, prev: 0.0 }
    }

    /// Appends the resampled `input` to `out`. Positions index a virtual
    /// signal where 0 is the last sample of the previous call.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        let prev = self.prev;
        let at = |i: usize| if i == 0 { prev } else { input[i - 1] };
        let n = input.len() as f64;
        while self.pos < n {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = input[input.len() - 1];
    }
}

/// Second-order low-pass (RBJ cookbook), used for walls and diffraction.
#[derive(Clone)]
pub struct LowPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
    cutoff: f32,
}

impl LowPass {
    /// Above this the filter is bypassed.
    pub const OPEN: f32 = 18_000.0;

    pub fn new() -> Self {
        let mut f = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0, cutoff: 0.0 };
        f.set_cutoff(Self::OPEN);
        f
    }

    pub fn set_cutoff(&mut self, hz: f32) {
        let hz = hz.clamp(80.0, Self::OPEN);
        if (hz - self.cutoff).abs() < 1.0 {
            return;
        }
        self.cutoff = hz;
        let w = 2.0 * std::f32::consts::PI * hz / RATE;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        self.b0 = (1.0 - cos) / 2.0 / a0;
        self.b1 = (1.0 - cos) / a0;
        self.b2 = self.b0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.cutoff >= Self::OPEN {
            return;
        }
        for x in buf {
            // Transposed direct form II.
            let y = self.b0 * *x + self.z1;
            self.z1 = self.b1 * *x - self.a1 * y + self.z2;
            self.z2 = self.b2 * *x - self.a2 * y;
            *x = y;
        }
    }
}

/// Exponential smoothing towards a target, once per audio frame.
#[derive(Clone, Copy)]
pub struct Smooth {
    pub value: f32,
    coef: f32,
}

impl Smooth {
    /// `frames`: frames to cover ~63% of a step.
    pub fn new(value: f32, frames: f32) -> Self {
        Self { value, coef: 1.0 - (-1.0 / frames.max(1.0)).exp() }
    }

    pub fn step(&mut self, target: f32) -> f32 {
        self.value += (target - self.value) * self.coef;
        self.value
    }
}

/// Schroeder all-pass: smears a sound in time without colouring it.
struct AllPass {
    buf: Vec<f32>,
    idx: usize,
    gain: f32,
}

impl AllPass {
    fn new(len: usize, gain: f32) -> Self {
        Self { buf: vec![0.0; len], idx: 0, gain }
    }

    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.idx];
        let v = x + b * self.gain;
        self.buf[self.idx] = v;
        self.idx = (self.idx + 1) % self.buf.len();
        b - v * self.gain
    }
}

const LINES: usize = 16;
/// Longest delay a line can reach (samples, a power of two: 170 ms).
const LINE_MAX: usize = 8192;
/// Line lengths (ms) in a middling room: spread out, no two in a simple
/// ratio. 16 lines adding up to a second: so many resonances so close
/// together that none stands out (8 short ones sounded like a metal pipe,
/// in game 2026-11-20).
const LINE_MS: [f32; LINES] =
    [29.3, 31.9, 34.7, 37.9, 41.3, 44.9, 48.9, 53.3, 58.1, 63.3, 68.9, 75.1, 81.7, 89.0, 96.9, 105.5];
/// Early reflections: when (times the first one) and how loud, left/right.
const TAPS: [(f32, f32, f32); 10] = [
    (1.00, 0.90, 0.45),
    (1.19, 0.40, 0.85),
    (1.37, 0.75, 0.35),
    (1.61, 0.30, 0.70),
    (1.83, 0.60, 0.55),
    (2.09, 0.25, 0.55),
    (2.41, 0.50, 0.20),
    (2.73, 0.20, 0.42),
    (3.07, 0.35, 0.30),
    (3.43, 0.22, 0.25),
];
/// What a voice keeps of its past for the early reflections (340 ms).
const HISTORY: usize = 16384;
const SPEED_OF_SOUND: f32 = 343.0;

/// In-place fast Hadamard transform, normalised: every line feeds every
/// other one, energy kept.
fn hadamard(x: &mut [f32; LINES]) {
    let mut h = 1;
    while h < LINES {
        for i in (0..LINES).step_by(h * 2) {
            for j in i..i + h {
                let (a, b) = (x[j], x[j + h]);
                (x[j], x[j + h]) = (a + b, a - b);
            }
        }
        h *= 2;
    }
    let k = 1.0 / (LINES as f32).sqrt();
    x.iter_mut().for_each(|v| *v *= k);
}

/// A place's mean free path (m) from CET's `size` (0..1).
fn free_path(size: f32) -> f32 {
    2.0 + 25.0 * size.clamp(0.0, 1.0)
}

/// How long a place rings (RT60, seconds, the low and mid tones) from its
/// size (0..1, from the mean free path CET measures: 2 m + 25 m * size)
/// and how much its surfaces soak up (`damp` 0.3 bare concrete .. 0.55
/// furnished, covered). Sabine's law (RT60 = 0.161 V / (S a), V / S = mean
/// free path / 4), half as long again: the game's metres are short, and
/// at Sabine's own the tails ended too soon (in game, 2026-11-20).
pub fn ring_time(size: f32, damp: f32) -> f32 {
    let size = size.clamp(0.0, 1.0);
    // Small places are mostly furnished rooms (CET's `damp` only knows
    // what covers them): the smaller, the more they soak up.
    let absorption = 0.04 + (damp.clamp(0.0, 1.0) - 0.3).max(0.0) + 0.25 * (1.0 - size).powi(4);
    (0.06 * free_path(size) / absorption).clamp(0.5, 5.0)
}

/// How much shorter the highs ring: bare concrete keeps them, cloth and
/// furniture swallow them (and the air, over a long tail).
fn highs_ratio(damp: f32) -> f32 {
    (0.65 - 1.2 * (damp.clamp(0.0, 1.0) - 0.3)).clamp(0.3, 0.65)
}

/// A place's reverberation: its first reflections off the walls, then a
/// dense tail from 16 delay lines mixed into each other (a feedback delay
/// network, Jot's), ringing as long as the place does, its highs fading
/// first. Mono in, stereo wet out.
pub struct Reverb {
    lines: Vec<Vec<f32>>,
    write: usize,
    /// Each line's length now (samples), moving toward `target` slowly.
    len: [usize; LINES],
    target: [usize; LINES],
    /// Each line's loss per pass: lows (`gain`), and the one-pole filter
    /// that makes the highs lose more (`pole`), and its state.
    gain: [f32; LINES],
    pole: [f32; LINES],
    state: [f32; LINES],
    diffuse: Vec<AllPass>,
    history: Vec<f32>,
    at: usize,
    /// When the first reflection comes (samples).
    first: usize,
    input_gain: f32,
    tick: u32,
    size: Smooth,
    soak: Smooth,
}

impl Reverb {
    pub fn new() -> Self {
        let mut r = Self {
            lines: vec![vec![0.0; LINE_MAX]; LINES],
            write: 0,
            len: [0; LINES],
            target: [0; LINES],
            gain: [0.0; LINES],
            pole: [0.0; LINES],
            state: [0.0; LINES],
            // 2 to 8 ms: each reflection reaches the tail as a wash, not a click.
            diffuse: [142, 107, 379, 277].iter().map(|&n| AllPass::new(n, 0.6)).collect(),
            history: vec![0.0; HISTORY],
            at: 0,
            first: 1,
            input_gain: 0.0,
            tick: 0,
            size: Smooth::new(0.3, 25.0),
            soak: Smooth::new(0.4, 25.0),
        };
        r.tune(0.3, 0.4);
        r.len = r.target;
        r
    }

    /// The place: `size` 0..1 and `damp` 0..1 as CET measures them
    /// (see [`ring_time`]). Changes glide over about half a second.
    pub fn set_room(&mut self, size: f32, damp: f32) {
        let (size, damp) = (self.size.step(size.clamp(0.0, 1.0)), self.soak.step(damp.clamp(0.0, 1.0)));
        self.tune(size, damp);
    }

    fn tune(&mut self, size: f32, damp: f32) {
        let path = free_path(size);
        // Bigger places, longer lines: their echoes come further apart.
        let stretch = (0.7 + 0.03 * path).clamp(0.7, 1.55);
        let (rt, rt_high) = (ring_time(size, damp), ring_time(size, damp) * highs_ratio(damp));
        for i in 0..LINES {
            self.target[i] = ((LINE_MS[i] * stretch * RATE / 1000.0) as usize).clamp(64, LINE_MAX - 2);
            // Jot: per pass, the lows lose what makes `rt`, the highs `rt_high`.
            let n = self.target[i] as f32;
            let (lo, hi) = (10f32.powf(-3.0 * n / (rt * RATE)), 10f32.powf(-3.0 * n / (rt_high * RATE)));
            self.gain[i] = lo;
            self.pole[i] = (lo - hi) / (lo + hi);
        }
        // The first reflection after a trip to the walls and back, never under
        // 20 ms: sooner, our own voice (heard live through our head) and its
        // echo comb into a metallic flutter (in game, 2026-11-20).
        let first = (path / SPEED_OF_SOUND).max(0.02);
        // The last early reflection must still be in the history.
        self.first = ((first * RATE) as usize).min(((HISTORY - 2) as f32 / TAPS[TAPS.len() - 1].0) as usize);
        // Longer tails hold more energy: grow with the ring time, gently.
        self.input_gain = 0.9 / rt.powf(0.1);
    }

    /// Adds the wet signal for `input` (mono) into interleaved stereo `out`.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) {
        let mask = HISTORY - 1;
        for (x, o) in input.iter().zip(out.chunks_exact_mut(2)) {
            self.history[self.at] = *x * self.input_gain;
            let past = |k: usize| self.history[(self.at + HISTORY - k) & mask];
            // First reflections: a few distinct ones, left and right.
            let (mut l, mut r) = (0.0, 0.0);
            for (t, gl, gr) in TAPS {
                let v = past((self.first as f32 * t) as usize);
                l += v * gl;
                r += v * gr;
            }
            // The tail starts with them, smeared.
            let mut d = past(self.first) * 0.5;
            for ap in &mut self.diffuse {
                d = ap.process(d);
            }
            self.at = (self.at + 1) & mask;
            // A place that changed: the lines stretch a sample at a time.
            self.tick = self.tick.wrapping_add(1);
            if self.tick % 16 == 0 {
                for i in 0..LINES {
                    self.len[i] = match self.len[i].cmp(&self.target[i]) {
                        std::cmp::Ordering::Less => self.len[i] + 1,
                        std::cmp::Ordering::Greater => self.len[i] - 1,
                        std::cmp::Ordering::Equal => self.len[i],
                    };
                }
            }
            let mut y = [0.0f32; LINES];
            for i in 0..LINES {
                let v = self.lines[i][(self.write + LINE_MAX - self.len[i]) & (LINE_MAX - 1)];
                self.state[i] = v * self.gain[i] * (1.0 - self.pole[i]) + self.state[i] * self.pole[i];
                y[i] = self.state[i];
            }
            // Two different mixes of the lines: wide, each ear its own.
            for (i, v) in y.iter().enumerate() {
                l += v * if i % 2 == 0 { 0.3 } else { -0.3 };
                r += v * if i % 4 < 2 { 0.3 } else { -0.3 };
            }
            o[0] += l * 0.5;
            o[1] += r * 0.5;
            hadamard(&mut y);
            for i in 0..LINES {
                self.lines[i][self.write] = y[i] + d;
            }
            self.write = (self.write + 1) & (LINE_MAX - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowpass_attenuates_highs_keeps_lows() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..4800).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / RATE).sin()).collect()
        };
        let mut f = LowPass::new();
        f.set_cutoff(500.0);
        let mut low = tone(100.0);
        f.process(&mut low);
        let mut f = LowPass::new();
        f.set_cutoff(500.0);
        let mut high = tone(6000.0);
        f.process(&mut high);
        assert!(rms_db(&low[2400..]) > -4.0);
        assert!(rms_db(&high[2400..]) < -40.0);
    }

    #[test]
    fn reverb_tail_rings_and_decays() {
        let mut r = Reverb::new();
        r.set_room(0.9, 0.3);
        let mut input = vec![0.0f32; 48_000];
        input[0] = 1.0;
        let mut out = vec![0.0f32; 96_000];
        r.process(&input, &mut out);
        let early = rms_db(&out[2_000..20_000]);
        let late = rms_db(&out[80_000..96_000]);
        assert!(early > -80.0, "tail present: {early}");
        assert!(late < early - 10.0, "tail decays: {early} -> {late}");
    }

    #[test]
    fn resampler_ratio() {
        let mut r = Resampler::new(32_000, 48_000);
        let mut out = Vec::new();
        for _ in 0..10 {
            r.process(&[0.5; 320], &mut out);
        }
        assert!((out.len() as i32 - 4800).abs() <= 1);
    }

    /// Ring time from an impulse (Schroeder integration, -5..-25 dB).
    fn measured_ring(size: f32, damp: f32) -> f32 {
        let mut r = Reverb::new();
        for _ in 0..400 {
            r.set_room(size, damp);
        }
        let mut x = vec![0.0f32; 48_000 * 6];
        x[0] = 1.0;
        let mut o = vec![0.0f32; x.len() * 2];
        r.process(&x, &mut o);
        let e: Vec<f64> = o.chunks(2).map(|c| f64::from(c[0]).powi(2) + f64::from(c[1]).powi(2)).collect();
        let mut acc = vec![0.0f64; e.len() + 1];
        for i in (0..e.len()).rev() {
            acc[i] = acc[i + 1] + e[i];
        }
        let below = |db: f64| (0..e.len()).find(|&i| 10.0 * (acc[i] / acc[0]).log10() < db).unwrap_or(e.len());
        3.0 * (below(-25.0) - below(-5.0)) as f32 / RATE
    }

    #[test]
    fn a_tunnel_rings_for_seconds_a_room_briefly() {
        let tunnel = measured_ring(0.32, 0.3);
        let room = measured_ring(0.05, 0.5);
        println!("tunnel {tunnel:.2} s (asked {:.2}), room {room:.2} s (asked {:.2})", ring_time(0.32, 0.3), ring_time(0.05, 0.5));
        assert!(tunnel > 3.5, "tunnel {tunnel:.2} s");
        assert!(room < 0.8, "room {room:.2} s");
        assert!((ring_time(0.32, 0.3) - tunnel).abs() < 1.2);
    }
}
#[cfg(test)]
mod density {
    use super::{RATE, Reverb};

    /// How grainy a tail is: the spread (dB) of its 1 ms loudness around a
    /// steady decay, 60..440 ms after an impulse. Freeverb's: 30-42 dB, a
    /// few loud echoes with silence between (heard as a metallic buzz); a
    /// smooth wash, a few dB.
    fn ripple(size: f32, damp: f32) -> f32 {
        let mut r = Reverb::new();
        for _ in 0..400 {
            r.set_room(size, damp);
        }
        let mut x = vec![0.0f32; 24_000];
        x[0] = 1.0;
        let mut o = vec![0.0f32; 48_000];
        r.process(&x, &mut o);
        let ms = (RATE / 1000.0) as usize;
        let lg: Vec<f32> = (60..440)
            .map(|k| {
                let e = o.chunks(2).skip(k * ms).take(ms).map(|c| c[0] * c[0]).sum::<f32>() / ms as f32;
                10.0 * e.max(1e-24).log10()
            })
            .collect();
        let n = lg.len() as f32;
        let (mx, my) = ((n - 1.0) / 2.0, lg.iter().sum::<f32>() / n);
        let slope = lg.iter().enumerate().map(|(i, y)| (i as f32 - mx) * (y - my)).sum::<f32>()
            / lg.iter().enumerate().map(|(i, _)| (i as f32 - mx).powi(2)).sum::<f32>();
        (lg.iter().enumerate().map(|(i, y)| (y - my - slope * (i as f32 - mx)).powi(2)).sum::<f32>() / n).sqrt()
    }

    /// Wet level for steady noise (dB under the input): Freeverb's was 2 dB.
    fn level(size: f32, damp: f32) -> f32 {
        let mut r = Reverb::new();
        let mut seed = 1u32;
        let (mut e, mut ein) = (0.0f64, 0.0f64);
        for k in 0..300 {
            r.set_room(size, damp);
            let x: Vec<f32> = (0..960)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.2
                })
                .collect();
            let mut o = vec![0.0f32; 1920];
            r.process(&x, &mut o);
            if k > 150 {
                e += o.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / 2.0;
                ein += x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
            }
        }
        10.0 * (e / ein).log10() as f32
    }

    /// How coloured a tail sounds (a pipe, a metal tube): the spread (dB)
    /// of its spectrum, 200 Hz to 5 kHz, smoothed over third-octaves and
    /// with its overall tilt taken out, 100 to 700 ms after an impulse.
    fn coloration(size: f32, damp: f32) -> f32 {
        use realfft::RealFftPlanner;
        let mut r = Reverb::new();
        for _ in 0..400 {
            r.set_room(size, damp);
        }
        let mut x = vec![0.0f32; 48_000];
        x[0] = 1.0;
        let mut o = vec![0.0f32; 96_000];
        r.process(&x, &mut o);
        let n = 28_800;
        let mut seg: Vec<f32> = o.chunks(2).skip(4_800).take(n).map(|c| c[0]).collect();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let mut spec = fft.make_output_vec();
        fft.process(&mut seg, &mut spec).unwrap();
        let hz = RATE / n as f32;
        let bands: Vec<(f32, f32)> = (0..)
            .map(|k| 200.0 * 2f32.powf(k as f32 / 6.0))
            .take_while(|f| *f < 5_000.0)
            .map(|f| {
                let (a, b) = ((f * 0.891 / hz) as usize, (f * 1.122 / hz) as usize);
                let e = spec[a..b].iter().map(|c| c.norm_sqr()).sum::<f32>() / (b - a) as f32;
                (f.log2(), 10.0 * e.log10())
            })
            .collect();
        let m = bands.len() as f32;
        let (mx, my) = (bands.iter().map(|b| b.0).sum::<f32>() / m, bands.iter().map(|b| b.1).sum::<f32>() / m);
        let slope = bands.iter().map(|(x, y)| (x - mx) * (y - my)).sum::<f32>() / bands.iter().map(|(x, _)| (x - mx).powi(2)).sum::<f32>();
        (bands.iter().map(|(x, y)| (y - my - slope * (x - mx)).powi(2)).sum::<f32>() / m).sqrt()
    }

    #[test]
    fn the_tail_is_a_smooth_wash_at_the_old_level() {
        for (size, damp) in [(0.05, 0.4), (0.15, 0.3), (0.32, 0.3)] {
            let (rp, lv, col) = (ripple(size, damp), level(size, damp), coloration(size, damp));
            println!("size {size} damp {damp}: ripple {rp:.1} dB, level {lv:+.1} dB, colour {col:.2} dB");
            assert!(rp < 4.0, "grainy: {rp:.1} dB");
            assert!((-8.0..3.0).contains(&lv), "level {lv:+.1} dB");
        }
    }
}
