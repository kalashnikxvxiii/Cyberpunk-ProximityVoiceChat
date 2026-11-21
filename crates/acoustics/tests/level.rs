//! Steam Audio's HRTF against the classic binaural model the mod used: same
//! loudness to the ear for a voice in front, so switching doesn't change anyone's volume.

use vc_acoustics::Engine;
use vc_acoustics::render::{Decoder, Heard, Voice};
use vc_acoustics::sa::{Direct, v3};
use vc_acoustics::world::Blocks;
use vc_dsp::binaural::Binaural;

fn noise(seed: &mut u32, out: &mut [f32]) {
    for x in out {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *x = ((*seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.2;
    }
}

fn db(sum: f64, n: usize) -> f32 {
    10.0 * (sum / n as f64).log10() as f32
}

/// dB of Steam Audio's direct path over the classic one, voice at `dir`
/// (listener axes: x right, y up, -z ahead for Steam; classic: x right, y ahead, z up).
fn steam_over_classic(steam_dir: [f32; 3], classic_dir: [f32; 3]) -> f32 {
    let e = Engine::new(Box::new(Blocks::default())).unwrap();
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    let h = Heard {
        gain: 1.0,
        direct: Direct { occlusion: 1.0, transmission: [1.0; 3] },
        dir: v3(steam_dir[0], steam_dir[1], steam_dir[2]),
        air: [1.0; 3],
        rebounds: 0.0,
        bends: [None; 2],
    };
    let mut classic = Binaural::new();
    let (mut seed, mut input) = (3u32, vec![0.0f32; 960]);
    let (mut s, mut c, mut n) = (0.0f64, 0.0f64, 0usize);
    for k in 0..40 {
        noise(&mut seed, &mut input);
        let mut a = vec![0.0f32; 1920];
        v.render(&e, None, &h, &input, &mut a, &mut dec.ambi);
        let mut b = vec![0.0f32; 1920];
        classic.process(&input, classic_dir, std::f32::consts::FRAC_1_SQRT_2, &mut b);
        if k >= 5 {
            s += a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            c += b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            n += 1920;
        }
    }
    db(s, n) - db(c, n)
}

#[test]
fn steam_hrtf_is_as_loud_as_the_classic_model() {
    let front = steam_over_classic([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);
    let side = steam_over_classic([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    println!("Steam over classic: front {front:+.1} dB, side {side:+.1} dB");
    // 3 dB under in energy sounds as loud (see `render::HRTF_LEVEL`).
    assert!((front + 3.0).abs() < 1.5 && (side + 3.0).abs() < 1.5, "front {front:+.1} dB, side {side:+.1} dB");
}

/// Each ear's level and delay for a voice at `az` degrees to the right
/// (0 ahead), elevation 0: (left-right level dB, right-ear lead in samples).
fn cues(steam: bool, az: f32) -> (f32, i32) {
    let (s, c) = az.to_radians().sin_cos();
    let e = Engine::new(Box::new(Blocks::default())).unwrap();
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    let h = Heard { gain: 1.0, direct: Direct { occlusion: 1.0, transmission: [1.0; 3] }, dir: v3(s, 0.0, -c), air: [1.0; 3], rebounds: 0.0, bends: [None; 2] };
    let mut classic = Binaural::new();
    let (mut seed, mut input) = (3u32, vec![0.0f32; 960]);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    for k in 0..40 {
        noise(&mut seed, &mut input);
        let mut a = vec![0.0f32; 1920];
        if steam {
            v.render(&e, None, &h, &input, &mut a, &mut dec.ambi);
        } else {
            classic.process(&input, [s, c, 0.0], std::f32::consts::FRAC_1_SQRT_2, &mut a);
        }
        if k >= 5 {
            for lr in a.chunks_exact(2) {
                l.push(lr[0]);
                r.push(lr[1]);
            }
        }
    }
    let en = |x: &[f32]| x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let ild = 10.0 * (en(&r) / en(&l)).log10() as f32;
    let corr = |lag: i32| (2000..l.len() - 2000).map(|i| f64::from(l[i]) * f64::from(r[(i as i32 - lag) as usize])).sum::<f64>();
    let itd = (-60..=60).max_by(|a, b| corr(*a).total_cmp(&corr(*b))).unwrap();
    (ild, itd)
}

#[test]
#[ignore]
fn where_each_model_puts_a_voice() {
    for az in [0.0, 15.0, -15.0, 30.0, -30.0, 90.0, -90.0, 180.0] {
        let (si, st) = cues(true, az);
        let (ci, ct) = cues(false, az);
        println!("{az:>5}°  steam: level {si:+5.1} dB, delay {st:+3}   classic: level {ci:+5.1} dB, delay {ct:+3}");
    }
}
