//! Where a voice is heard from: distance falloff, its direction relative to
//! where the listener looks (rendered by `binaural`), and party "radio".
//!
//! Coordinates are the game's: metres, Z up.

pub type Vec3 = [f32; 3];
pub use crate::binaural::Dir;

/// Within this distance a voice is at full volume. The game's metres are
/// short (a step covers one): 3 of them sound like 2 real ones, and so on.
pub const NEAR: f32 = 3.0;
/// Party members quieter than this through the air (about 18 m in the open,
/// or behind walls) come through as a radio instead...
pub const RADIO_BELOW: f32 = NEAR / 18.0;
/// ...and back in the air only once this much louder: standing at the edge doesn't flip it.
const RADIO_BACK: f32 = 1.2;
/// ...at this volume.
pub const RADIO_GAIN: f32 = 0.7;
/// Past the edge of its range a voice doesn't stop: it fades out over this
/// much further again (8 dB more, squared: gently at first). A cut at the
/// edge sounded like the voice switched off (in game, 2026-11-20).
pub const FADE_TAIL: f32 = 2.5;

pub fn distance(a: Vec3, b: Vec3) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// Sound in the open: -6 dB per doubling of distance past [`NEAR`], faded to
/// silence over the last [`FADE`] of `range` (where it drops under the noise).
pub fn falloff(dist: f32, range: f32) -> f32 {
    audible(NEAR / dist.max(NEAR), range)
}

/// A voice arriving at `gain` (distance and obstacles together), faded out as
/// it nears the quietest one still heard over the noise here: as quiet as a
/// voice in the open at the end of `range`. So a window or a wall shortens
/// the range just as distance and noise do, instead of cutting off.
pub fn audible(gain: f32, range: f32) -> f32 {
    fade(gain, NEAR / range.max(NEAR))
}

/// `gain` as is down to `floor` (the edge of the range), then faded out
/// over the next [`FADE_TAIL`]: gently at first, silent at its end.
fn fade(gain: f32, floor: f32) -> f32 {
    if gain >= floor {
        return gain;
    }
    let t = (1.0 - (floor / gain.max(1e-9)).ln() / FADE_TAIL.ln()).clamp(0.0, 1.0);
    gain * t * t
}

/// How much a place keeps a voice together, from its reverb (`wet`, 0..1):
/// 0 in the open, 1 in a bare tunnel, car park or hall.
pub fn carry(wet: f32) -> f32 {
    ((wet - 0.1) / 0.25).clamp(0.0, 1.0)
}

/// Air absorbs the highs over long distances (a far shout sounds thin).
pub fn air_cutoff(dist: f32) -> f32 {
    (1.2e6 / dist.max(1.0)).min(crate::LowPass::OPEN)
}

/// Straight ahead, and where a voice with no place (the radio) is heard.
pub const AHEAD: Dir = [0.0, 1.0, 0.0];

/// Ears above the feet (the game's positions) when the real height isn't known: standing.
pub const EAR_HEIGHT: f32 = 1.6;

/// Where the ears are, `height` above `feet` (lower crouching or seated).
pub fn ear(feet: Vec3, height: f32) -> Vec3 {
    [feet[0], feet[1], feet[2] + height]
}

/// Direction of `src` relative to the head looking along `forward` (the
/// view, pitch included): x right, y front, z up. Very close voices: ahead.
/// Right is forward's horizontal (fx, fy) turned to (fy, -fx); up = right × forward.
pub fn head_dir(listener: Vec3, forward: Vec3, src: Vec3) -> Dir {
    let d = [src[0] - listener[0], src[1] - listener[1], src[2] - listener[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let flen = (forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2]).sqrt();
    let hlen = (forward[0] * forward[0] + forward[1] * forward[1]).sqrt();
    if len < 0.5 || hlen < 1e-6 {
        return AHEAD;
    }
    let f = forward.map(|v| v / flen);
    let r = [forward[1] / hlen, -forward[0] / hlen, 0.0];
    let u = [r[1] * f[2], -r[0] * f[2], r[0] * f[1] - r[1] * f[0]];
    let dot = |a: Vec3| (d[0] * a[0] + d[1] * a[1] + d[2] * a[2]) / len;
    [dot(r), dot(f), dot(u)]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Heard {
    pub gain: f32,
    pub dir: Dir,
    /// Low-pass cutoff in Hz (walls, doorways, radio).
    pub cutoff: f32,
    /// True when heard through the party radio rather than the air.
    pub radio: bool,
    /// How much of it goes to the room's reverb: falls off slower than the voice.
    pub reverb: f32,
}

/// How a voice arrives through the air, as computed by the acoustics
/// (raycasts in the game): the point it seems to come from (a doorway, or
/// the speaker itself), extra attenuation and muffling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Path {
    pub from: Vec3,
    pub gain: f32,
    pub cutoff: f32,
}

impl Path {
    pub fn direct(src: Vec3) -> Self {
        Self { from: src, gain: 1.0, cutoff: crate::LowPass::OPEN }
    }
}

/// Telephone band for the party radio.
pub const RADIO_CUTOFF: f32 = 3_400.0;

/// `range`: where this voice fades out in the open, after shouting and noise.
/// `carry`: see [`carry`]. `was_radio`: how it was heard a moment ago.
pub fn heard(listener: Vec3, forward: Vec3, path: Path, party: bool, range: f32, carry: f32, was_radio: bool) -> Heard {
    let dist = distance(listener, path.from);
    // Walls all around keep the sound together: in a bare tunnel it carries
    // as far again as in the open (as if half the distance).
    let spread = NEAR / (dist / (1.0 + carry)).max(NEAR);
    let gain = audible(spread * path.gain, range);
    let below = if was_radio { RADIO_BELOW * RADIO_BACK } else { RADIO_BELOW };
    if party && gain < below {
        return Heard { gain: RADIO_GAIN, dir: AHEAD, cutoff: RADIO_CUTOFF, radio: true, reverb: 0.0 };
    }
    let dir = head_dir(listener, forward, path.from);
    // The room's echo builds up all along the way: it falls off half as fast as the voice.
    let reverb = if spread > 0.0 { gain / spread.sqrt() } else { 0.0 };
    Heard { gain, dir, cutoff: path.cutoff.min(air_cutoff(dist)), radio: false, reverb }
}

/// Background noise where the listener is, from the game's own sound in the
/// voice band (dBFS, see `scene`): 0 = quiet, 1 = club music, traffic.
pub fn noise(db: f32) -> f32 {
    let t = ((db + 50.0) / 25.0).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Noise covers far voices: at full noise the range shrinks by this much.
const NOISE_MASK_DB: f32 = 6.0;

/// Range multiplier for the listener's noise (1 in quiet, half in a club).
pub fn noise_reach(noise: f32) -> f32 {
    10f32.powf(-NOISE_MASK_DB * noise.clamp(0.0, 1.0) / 20.0)
}

/// How much louder than usual someone talks, from their microphone level.
/// Both levels hold syllable peaks (attack at once, fall in 300 ms), so a
/// usual sentence reads as 0. The usual level is learnt over the first
/// seconds of speech (reading 0 meanwhile, so a quiet first breath can't make
/// every word after it a shout), then follows slowly, rising much slower than
/// it falls, so a few shouts don't become the norm.
#[derive(Debug, Default)]
pub struct Shout {
    usual: Option<f32>,
    now: f32,
    /// Speech frames learnt from so far.
    learnt: u32,
}

impl Shout {
    /// Below the usual peaks by this much: a pause, not speech.
    const PAUSE_DB: f32 = 12.0;
    /// Natural swing of a calm voice, not counted as raising it.
    const SWING_DB: f32 = 3.0;

    /// Speech frames (3 s) to learn the usual level before reading shouts.
    const LEARN: u32 = 150;

    /// Already knowing the usual level (peaks, dBFS).
    pub fn with_usual(db: f32) -> Self {
        Self { usual: Some(db), now: db, learnt: Self::LEARN }
    }

    /// Follows syllable peaks: the level `push` compares.
    pub fn peak(&mut self, db: f32) -> f32 {
        self.now = if db > self.now { db } else { self.now + (db - self.now) * (20.0 / 300.0) };
        self.now
    }

    /// One 20 ms frame being sent, its level in dBFS: dB over the usual voice.
    pub fn push(&mut self, db: f32) -> f32 {
        if db < -60.0 {
            self.now = db;
            return 0.0;
        }
        let now = self.peak(db);
        let usual = *self.usual.get_or_insert(now);
        if self.learnt < Self::LEARN {
            // The louder half of what we hear is speech; pauses only pull it down slowly.
            let k = if now > usual { 1.0 / 20.0 } else { 1.0 / 100.0 };
            self.usual = Some(usual + (now - usual) * k);
            self.learnt += 1;
            return 0.0;
        }
        if now > usual - Self::PAUSE_DB {
            // In 20 ms frames: about 30 s to rise, 5 s to fall.
            let k = if now > usual { 1.0 / 1500.0 } else { 1.0 / 250.0 };
            self.usual = Some(usual + (now - usual) * k);
        }
        (now - usual - Self::SWING_DB).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // vc_protocol::RANGE, the open-air range.
    const RANGE: f32 = 72.0;

    const FWD_Y: Vec3 = [0.0, 1.0, 0.0];

    #[test]
    fn right_is_right_and_back_is_back() {
        // Facing +Y, +X is to the right.
        assert!(head_dir([0.0; 3], FWD_Y, [5.0, 0.0, 0.0])[0] > 0.99);
        assert!(head_dir([0.0; 3], FWD_Y, [-5.0, 0.0, 0.0])[0] < -0.99);
        let behind = head_dir([0.0; 3], FWD_Y, [0.0, -5.0, 0.0]);
        assert!(behind[0].abs() < 0.01 && behind[1] < -0.99);
        assert!(head_dir([0.0; 3], FWD_Y, [0.0, 3.0, 3.0])[2] > 0.7, "up is up");
        // Looking 45 degrees up, a voice straight ahead in the world is below the head.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let low = head_dir([0.0; 3], [0.0, s, s], [0.0, 5.0, 0.0]);
        assert!((low[1] - s).abs() < 0.01 && (low[2] + s).abs() < 0.01 && low[0].abs() < 0.01);
    }

    #[test]
    fn falloff_and_party_radio() {
        let at = |x: f32, party: bool| heard([0.0; 3], FWD_Y, Path::direct([x, 0.0, 0.0]), party, RANGE, 0.0, false);
        assert!(at(1.0, false).gain > 0.9 && at(1.0, false).dir[0] > 0.99);
        assert_eq!(at(RANGE * FADE_TAIL, false).gain, 0.0);
        let far_party = at(90.0, true);
        assert!(far_party.radio && far_party.dir == AHEAD);
        assert!(!at(3.0, true).radio, "near party members are heard in the air");
    }

    #[test]
    fn shouting_reads_above_the_usual_voice() {
        let mut s = Shout::default();
        // Ten seconds of calm talk swinging between -32 and -26 dBFS, with pauses.
        let mut last = 0.0;
        for i in 0..500 {
            let db = match i % 25 {
                0..=4 => -80.0,
                n => -32.0 + (n % 3) as f32 * 3.0,
            };
            last = s.push(db);
        }
        assert!(last < 1.0, "calm talk is not a shout: {last}");
        let shout = (0..20).map(|_| s.push(-12.0)).last().unwrap();
        // 14 dB over the calm peaks, less the calm swing.
        assert!((10.0..12.0).contains(&shout), "shout: {shout}");
        assert!(s.push(-80.0) == 0.0);
    }

    #[test]
    fn a_quiet_start_does_not_make_talk_a_shout() {
        let mut s = Shout::default();
        // A breath at -50 dBFS, then calm talk at -30.
        for _ in 0..25 {
            s.push(-50.0);
        }
        let talk = (0..400).map(|i| s.push(if i % 25 < 4 { -80.0 } else { -30.0 })).fold(0.0f32, f32::max);
        assert!(talk < 1.0, "calm talk read as +{talk} dB");
        assert!((0..20).map(|_| s.push(-14.0)).last().unwrap() > 10.0);
    }

    #[test]
    fn noise_shrinks_the_range() {
        assert_eq!(noise_reach(noise(-60.0)), 1.0);
        assert!(noise_reach(noise(-15.0)) < 0.55);
    }

    #[test]
    fn inverse_distance() {
        // -6 dB per doubling up to the range, then a gentle fade to nothing.
        assert!((falloff(6.0, RANGE) - 0.5).abs() < 1e-6);
        assert!((falloff(12.0, RANGE) - 0.25).abs() < 1e-6);
        assert!((falloff(RANGE, RANGE) - NEAR / RANGE).abs() < 1e-6);
        // No step past it: quieter and quieter, and 25% further only about
        // 6 dB under the edge (it used to be silent there)...
        let mut d = RANGE;
        while d < RANGE * FADE_TAIL {
            assert!(falloff(d * 1.05, RANGE) <= falloff(d, RANGE));
            d *= 1.05;
        }
        let edge = falloff(RANGE, RANGE);
        assert!(falloff(RANGE * 1.25, RANGE) > edge * 0.4);
        // ...silent at the end.
        assert_eq!(falloff(RANGE * FADE_TAIL, RANGE), 0.0);
        // A shout reaching 10x as far is still there at 300 m.
        assert!(falloff(300.0, RANGE * 10.0) > 0.009);
        assert!(air_cutoff(400.0) < 4_000.0 && air_cutoff(10.0) >= crate::LowPass::OPEN);
    }

    #[test]
    fn obstacles_shorten_the_range() {
        let through = |x: f32, gain: f32| {
            heard([0.0; 3], FWD_Y, Path { from: [x, 0.0, 0.0], gain, cutoff: 900.0 }, false, RANGE, 0.0, false).gain
        };
        // Behind a pane at -26 dB a voice is gone well before the open-air range...
        assert!(through(3.0, 0.05) > 0.0 && through(3.6 * FADE_TAIL, 0.05) == 0.0);
        // ...and fades out gradually, not at once.
        assert!(through(5.0, 0.05) > 0.0 && through(5.0, 0.05) < through(3.0, 0.05) * 0.5);
    }

    #[test]
    fn tunnels_carry_further_and_their_echo_further_still() {
        let at = |x: f32, carry: f32| heard([0.0; 3], FWD_Y, Path::direct([x, 0.0, 0.0]), false, RANGE, carry, false);
        assert_eq!(at(RANGE * FADE_TAIL, 0.0).gain, 0.0);
        let far = at(90.0, 1.0);
        assert!(far.gain > 0.05 && far.reverb > far.gain * 2.0, "{far:?}");
        // Close by nothing changes.
        assert!((at(3.0, 1.0).gain - at(3.0, 0.0).gain).abs() < 1e-6);
        // In the open too the echo outlasts the voice a little (for a room behind a door).
        let open = at(24.0, 0.0);
        assert!(open.reverb > open.gain * 2.0);
    }

    #[test]
    fn the_party_radio_does_not_flip_at_the_edge() {
        let at = |x: f32, was: bool| heard([0.0; 3], FWD_Y, Path::direct([x, 0.0, 0.0]), true, RANGE, 0.0, was).radio;
        assert!(at(19.0, false));
        // Coming back: still the radio until clearly closer (about 15 m).
        assert!(at(16.5, true) && !at(16.5, false));
        assert!(!at(14.0, true));
    }

    #[test]
    fn walls_silence_strangers_but_not_party() {
        let wall = Path { from: [3.0, 0.0, 0.0], gain: 0.0, cutoff: 300.0 };
        let stranger = heard([0.0; 3], FWD_Y, wall, false, RANGE, 0.0, false);
        assert_eq!(stranger.gain, 0.0);
        assert!(heard([0.0; 3], FWD_Y, wall, true, RANGE, 0.0, false).radio);
    }
}
