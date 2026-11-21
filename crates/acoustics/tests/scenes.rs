//! The cases from the in-game tests (V3 shop window, V4 doors, V6 tunnels),
//! built with boxes and run through the real Steam Audio: what reaches the
//! listener's ears, in dB.

use vc_acoustics::materials::{CONCRETE, DOOR, GLASS};
use vc_acoustics::render::{Decoder, Heard, Voice, direction};
use vc_acoustics::world::{Blocks, block, listener, to_sa};
use vc_acoustics::{Engine, QUALITY};

const FRAMES: usize = 70;

/// One engine at a time: several engines rebounding at once disturb each
/// other's results (in the game there is only one).
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Deterministic noise, voice-like level.
fn noise(seed: &mut u32, out: &mut [f32]) {
    for x in out {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *x = ((*seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.2;
    }
}

struct Result {
    total_db: f32,
    direct_db: f32,
    rebounds_db: f32,
    occlusion: f32,
}

/// Speaker at `s`, listener's ears at `l` facing `f` (game axes): the level
/// at the ears (all frames after the rebounds settled).
fn hear(world: &Blocks, s: [f32; 3], l: [f32; 3], f: [f32; 3]) -> Result {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let e = Engine::new(Box::new(world.clone())).unwrap();
    let space = listener(l, f);
    let voices = [(1u32, to_sa(s))];
    e.simulate(&space, &voices, &QUALITY);
    let d = e.direct_of(1).unwrap();
    let src = e.source(1).unwrap();
    // One pass (a source's rebounds feed one effect): direct and rebounds apart.
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    let h = Heard { gain: 1.0, direct: d, dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
    let (mut seed, mut input) = (7u32, vec![0.0; 960]);
    let (mut sd, mut sr, mut st, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let e2 = |x: &[f32]| x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>();
    for k in 0..FRAMES {
        noise(&mut seed, &mut input);
        let mut direct = vec![0.0f32; 1920];
        v.render(&e, Some(&src), &h, &input, &mut direct, &mut dec.ambi);
        let mut rebounds = vec![0.0f32; 1920];
        dec.decode(&e, &space, &mut rebounds);
        if k >= 20 {
            let total: Vec<f32> = direct.iter().zip(&rebounds).map(|(a, b)| a + b).collect();
            (sd, sr, st, n) = (sd + e2(&direct), sr + e2(&rebounds), st + e2(&total), n + 1920);
        }
    }
    let db = |x: f64| 10.0 * (x / n as f64).log10() as f32;
    let (total_db, direct_db, rebounds_db) = (db(st), db(sd), db(sr));
    Result { total_db, direct_db, rebounds_db, occlusion: d.occlusion }
}

fn ground() -> Vec<vc_acoustics::world::Block> {
    vec![block([-40.0, -40.0, -1.0], [50.0, 60.0, 0.0], CONCRETE)]
}

/// A diner: room x 0..10, y 0..8, 3 m high, concrete. The front wall (y = 0)
/// has a shop window (x 1..6, from 0.8 to 2.5 m) and a doorway (x 7.5..8.7),
/// open or with a closed door.
fn diner(door_closed: bool) -> Blocks {
    let t = 0.3;
    let mut b = ground();
    b.push(block([-t, -t, 0.0], [0.0, 8.0 + t, 3.0], CONCRETE)); // west
    b.push(block([10.0, -t, 0.0], [10.0 + t, 8.0 + t, 3.0], CONCRETE)); // east
    b.push(block([-t, 8.0, 0.0], [10.0 + t, 8.0 + t, 3.0], CONCRETE)); // back
    b.push(block([-t, -t, 3.0], [10.0 + t, 8.0 + t, 3.0 + t], CONCRETE)); // roof
    // Front wall, around the window and the doorway.
    b.push(block([-t, -t, 0.0], [1.0, 0.0, 3.0], CONCRETE));
    b.push(block([6.0, -t, 0.0], [7.5, 0.0, 3.0], CONCRETE));
    b.push(block([8.7, -t, 0.0], [10.0 + t, 0.0, 3.0], CONCRETE));
    b.push(block([1.0, -t, 0.0], [6.0, 0.0, 0.8], CONCRETE)); // under the window
    b.push(block([1.0, -t, 2.5], [6.0, 0.0, 3.0], CONCRETE)); // over the window
    b.push(block([7.5, -t, 2.2], [8.7, 0.0, 3.0], CONCRETE)); // over the door
    b.push(block([1.0, -0.16, 0.8], [6.0, -0.15, 2.5], GLASS)); // the pane
    if door_closed {
        b.push(block([7.5, -0.2, 0.0], [8.7, -0.15, 2.2], DOOR));
    }
    Blocks(b)
}

const INSIDE: [f32; 3] = [5.0, 6.0, 1.6];
const NORTH: [f32; 3] = [0.0, 1.0, 0.0];

#[test]
fn shop_window_is_much_quieter_than_the_door() {
    let w = diner(false);
    // Someone the doorway lets us see: in front of the window we only get
    // the pane and what comes out of the door.
    let s = [7.0, 6.0, 1.6];
    let window = hear(&w, s, [3.0, -2.5, 1.6], NORTH);
    let door = hear(&w, s, [8.1, -2.5, 1.6], NORTH);
    for (n, r) in [("window", &window), ("door", &door)] {
        println!("{n}: total {:.1}, direct {:.1}, rebounds {:.1}, occlusion {:.2}", r.total_db, r.direct_db, r.rebounds_db, r.occlusion);
    }
    assert!(window.occlusion < 0.1, "the pane hides the speaker");
    assert!(door.occlusion > 0.9, "the doorway shows them");
    assert!(door.total_db > window.total_db + 15.0);
}

#[test]
fn out_of_sight_the_door_still_beats_the_window() {
    let w = diner(false);
    let window = hear(&w, INSIDE, [3.0, -2.5, 1.6], NORTH);
    let door = hear(&w, INSIDE, [8.1, -2.5, 1.6], NORTH);
    for (n, r) in [("window", &window), ("door", &door)] {
        println!("{n}: total {:.1}, direct {:.1}, rebounds {:.1}, occlusion {:.2}", r.total_db, r.direct_db, r.rebounds_db, r.occlusion);
    }
    assert!(door.total_db > window.total_db + 4.0);
}

#[test]
fn a_closed_door_muffles_but_lets_some_through() {
    let open = hear(&diner(false), INSIDE, [8.1, -2.5, 1.6], NORTH);
    let closed = hear(&diner(true), INSIDE, [8.1, -2.5, 1.6], NORTH);
    let mut sealed = diner(true);
    // A concrete slab instead of the door: nothing gets through.
    sealed.0.push(block([7.5, -0.3, 0.0], [8.7, -0.21, 2.2], CONCRETE));
    sealed.0.push(block([1.0, -0.3, 0.8], [6.0, -0.2, 2.5], CONCRETE));
    let wall = hear(&sealed, INSIDE, [8.1, -2.5, 1.6], NORTH);
    for (n, r) in [("open", &open), ("closed", &closed), ("walled up", &wall)] {
        println!("{n}: total {:.1}, direct {:.1}, rebounds {:.1}", r.total_db, r.direct_db, r.rebounds_db);
    }
    assert!(closed.total_db < open.total_db - 6.0);
    assert!(closed.total_db > wall.total_db + 3.0);
}

#[test]
fn walking_along_the_window_changes_the_voice_gradually() {
    let w = diner(false);
    let heard: Vec<Result> = (0..6).map(|i| hear(&w, INSIDE, [4.0 + i as f32 * 0.1, -2.5, 1.6], NORTH)).collect();
    for r in &heard {
        println!("total {:.1}, direct {:.1}, rebounds {:.1}, occlusion {:.2}", r.total_db, r.direct_db, r.rebounds_db, r.occlusion);
    }
    let levels: Vec<f32> = heard.iter().map(|r| r.total_db).collect();
    println!("{levels:?}");
    // Steam Audio shoots its rays in new directions every run: up to about
    // ±1.5 dB of grain (1 run in 30 reached 3.3 dB at 512 rays), not a jump.
    for p in levels.windows(2) {
        assert!((p[0] - p[1]).abs() < 4.0, "{levels:?}");
    }
}

#[test]
fn a_seam_in_a_wall_is_not_a_way_through() {
    let s = [6.0, 0.0, 1.6];
    let l = [0.0, 0.0, 1.6];
    let solid = Blocks([ground(), vec![block([2.5, -30.0, 0.0], [3.5, 30.0, 10.0], CONCRETE)]].concat());
    let seam = Blocks(
        [ground(), vec![block([2.5, -30.0, 0.0], [3.5, -0.01, 10.0], CONCRETE), block([2.5, 0.01, 0.0], [3.5, 30.0, 10.0], CONCRETE)]].concat(),
    );
    let open = hear(&Blocks(ground()), s, l, [1.0, 0.0, 0.0]);
    let a = hear(&solid, s, l, [1.0, 0.0, 0.0]);
    let b = hear(&seam, s, l, [1.0, 0.0, 0.0]);
    println!("open {:.1}, wall {:.1}, wall with seam {:.1} (occl {:.2}, direct {:.1})", open.total_db, a.total_db, b.total_db, b.occlusion, b.direct_db);
    // The voice itself doesn't slip through (the seam guard on single rays);
    // a few rebounding rays do, faintly, as through a door left ajar.
    assert!((a.direct_db - b.direct_db).abs() < 3.0);
    // 15 dB: on white noise the rebounds' make-up gain (render::SHELF_GAIN)
    // counts 7 dB more than on a voice, and a run in ten leaks 10 dB more.
    assert!(b.total_db < open.total_db - 15.0);
}

#[test]
fn a_tunnel_rings_out_of_its_mouth() {
    // Tunnel along y from 0 to 40, 6 m wide, 5 m high.
    let mut t = ground();
    t.push(block([-3.5, 0.0, 0.0], [-3.0, 40.0, 5.0], CONCRETE));
    t.push(block([3.0, 0.0, 0.0], [3.5, 40.0, 5.0], CONCRETE));
    t.push(block([-3.5, 0.0, 5.0], [3.5, 40.0, 5.5], CONCRETE));
    let s = [0.0, 15.0, 1.6];
    let l = [0.0, -5.0, 1.6];
    let tunnel = hear(&Blocks(t), s, l, NORTH);
    let open = hear(&Blocks(ground()), s, l, NORTH);
    println!("rebounds: tunnel {:.1}, open {:.1}; direct {:.1}", tunnel.rebounds_db, open.rebounds_db, tunnel.direct_db);
    assert!(tunnel.rebounds_db > open.rebounds_db + 6.0);
}

#[test]
fn direct_and_rebounds_can_run_at_once() {
    // As in the game: the direct sound on one thread while the rebounds run on another.
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let e = Engine::new(Box::new(diner(false))).unwrap();
    let space = listener([3.0, -2.5, 1.6], NORTH);
    let voices = [(1u32, to_sa([7.0, 6.0, 1.6]))];
    e.simulate(&space, &voices, &QUALITY);
    let alone = e.direct_of(1).unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..5 {
                e.rebounds(&space, &voices, &QUALITY);
            }
        });
        for _ in 0..50 {
            e.direct(&space, &voices, &QUALITY);
            let d = e.direct_of(1).unwrap();
            assert!((d.occlusion - alone.occlusion).abs() < 0.13, "{d:?} vs {alone:?}");
        }
    });
    assert!(e.source(1).is_some());
}

#[test]
#[ignore]
fn spread_of_rebounds_between_runs() {
    let w = diner(false);
    let levels: Vec<f32> = (0..8).map(|_| hear(&w, INSIDE, [4.0, -2.5, 1.6], NORTH).rebounds_db).collect();
    println!("same place, 8 runs: {levels:?}");
}

/// A furnished-size concrete room, 6 x 5 x 3 m, closed.
fn room() -> Blocks {
    let t = 0.3;
    let mut b = ground();
    b.push(block([-t, -t, 0.0], [0.0, 5.0 + t, 3.0], CONCRETE));
    b.push(block([6.0, -t, 0.0], [6.0 + t, 5.0 + t, 3.0], CONCRETE));
    b.push(block([-t, -t, 0.0], [6.0 + t, 0.0, 3.0], CONCRETE));
    b.push(block([-t, 5.0, 0.0], [6.0 + t, 5.0 + t, 3.0], CONCRETE));
    b.push(block([-t, -t, 3.0], [6.0 + t, 5.0 + t, 3.0 + t], CONCRETE));
    Blocks(b)
}

#[test]
#[ignore]
fn rebounds_against_direct_in_a_room() {
    for d in [1.0f32, 2.0, 4.0] {
        let r = hear(&room(), [1.0, 2.5, 1.6], [1.0 + d, 2.5, 1.6], [1.0, 0.0, 0.0]);
        println!("room, {d} m: direct {:.1}, rebounds {:.1} (gain 1)", r.direct_db, r.rebounds_db);
    }
}

/// The rebounds' level is measured even while the voice isn't heard (or it
/// could never come back), and pauses between words don't raise it.
#[test]
fn rebound_level_survives_silence_and_pauses() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let w = room();
    let (s, l) = ([2.0, 2.0, 1.6], [5.0, 4.0, 1.6]);
    let e = Engine::new(Box::new(w)).unwrap();
    let space = listener(l, NORTH);
    e.simulate(&space, &[(1, to_sa(s))], &QUALITY);
    let (d, src) = (e.direct_of(1).unwrap(), e.source(1).unwrap());
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    // Not heard at all: no gain, no rebounds into the mix.
    let mute = Heard { gain: 0.0, direct: d, dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 0.0, bends: [None; 2] };
    let (mut seed, mut input, mut mix) = (7u32, vec![0.0; 960], vec![0.0; 1920]);
    for _ in 0..100 {
        noise(&mut seed, &mut input);
        v.render(&e, Some(&src), &mute, &input, &mut mix, &mut dec.ambi);
    }
    let talking = v.rebound_level();
    assert!(talking > 0.01, "measured while muted: {talking}");
    for _ in 0..50 {
        v.render(&e, Some(&src), &mute, &[0.0; 960], &mut mix, &mut dec.ambi);
    }
    assert!((v.rebound_level() - talking).abs() < talking * 0.05, "a pause keeps it: {talking} -> {}", v.rebound_level());
}

/// In front of the shop window, a speaker hidden inside: the rebounds come
/// from the doorway to our right; from east of the door, from our left.
#[test]
fn rebounds_come_from_the_doorway() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    for (l, east) in [([3.0, -2.5, 1.6], true), ([13.0, -2.5, 1.6], false)] {
        let s = [3.0, 5.0, 1.6];
        let e = Engine::new(Box::new(diner(false))).unwrap();
        let space = listener(l, NORTH);
        e.simulate(&space, &[(1, to_sa(s))], &QUALITY);
        let src = e.source(1).unwrap();
        let mut v = Voice::new(&e).unwrap();
        let mut dec = Decoder::new(&e).unwrap();
        let h = Heard { gain: 1.0, direct: e.direct_of(1).unwrap(), dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
        let (mut seed, mut input, mut mix) = (7u32, vec![0.0; 960], vec![0.0; 1920]);
        for _ in 0..60 {
            noise(&mut seed, &mut input);
            v.render(&e, Some(&src), &h, &input, &mut mix, &mut dec.ambi);
            dec.ambi.clear();
        }
        let a = v.arrival().unwrap();
        println!("from {l:?}: rebounds arrive along {:.2} {:.2} {:.2}", a.x, a.y, a.z);
        // Steam Audio's axes: x east, -z north.
        assert!(if east { a.x > 0.5 } else { a.x < -0.5 } && a.z < 0.0);
    }
}

#[test]
#[ignore]
fn walk_past_the_diner() {
    for s in [[7.0, 3.0, 1.6], [3.0, 5.0, 1.6]] {
        println!("speaker at {s:?}");
        for x in [-4.0f32, -1.0, 1.0, 3.0, 5.0, 6.5, 7.5, 8.1, 9.0, 11.0, 14.0] {
            let l = [x, -1.5, 1.6];
            let r = hear(&diner(false), s, l, NORTH);
            let d = ((s[0] - l[0]).powi(2) + (s[1] - l[1]).powi(2)).sqrt();
            println!("  x {x:5.1} dist {d:4.1}: direct {:6.1} rebounds {:6.1} (open air at this distance, Steam's law: {:6.1})", r.direct_db, r.rebounds_db, -20.0 * d.max(1.0).log10() - 3.0);
        }
    }
}

/// Rebounds over direct, octave by octave (gain 1 each), as heard at the
/// ears: no boom under 500 Hz (it was 9 to 12 dB over 1 kHz, see `render::SHELF_DB`).
#[test]
fn rebounds_dont_boom() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    for (world, s, l, name) in [(diner(false), [8.1, 4.0, 1.6], [8.1, 1.0, 1.6], "diner, 3 m"), (room(), [1.0, 2.5, 1.6], [4.0, 2.5, 1.6], "room, 3 m")] {
        let e = Engine::new(Box::new(world)).unwrap();
        let space = listener(l, NORTH);
        e.simulate(&space, &[(1, to_sa(s))], &QUALITY);
        let (d, src) = (e.direct_of(1).unwrap(), e.source(1).unwrap());
        let mut v = Voice::new(&e).unwrap();
        let mut dec = Decoder::new(&e).unwrap();
        let h = Heard { gain: 1.0, direct: d, dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
        let (mut seed, mut input) = (7u32, vec![0.0; 960]);
        let (mut dl, mut rl) = (Vec::new(), Vec::new());
        for k in 0..FRAMES {
            noise(&mut seed, &mut input);
            let mut direct = vec![0.0f32; 1920];
            v.render(&e, Some(&src), &h, &input, &mut direct, &mut dec.ambi);
            let mut rebounds = vec![0.0f32; 1920];
            dec.decode(&e, &space, &mut rebounds);
            if k >= 20 {
                dl.extend(direct.chunks_exact(2).map(|x| x[0]));
                rl.extend(rebounds.chunks_exact(2).map(|x| x[0]));
            }
        }
        let band = |x: &[f32], fc: f32| {
            let w = 2.0 * std::f32::consts::PI * fc / 48_000.0;
            let alpha = w.sin() / (2.0 * 1.4);
            let a0 = 1.0 + alpha;
            let (b0, b2, a1, a2) = (alpha / a0, -alpha / a0, -2.0 * w.cos() / a0, (1.0 - alpha) / a0);
            let (mut x1, mut x2, mut y1, mut y2, mut sum) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f64);
            for &x0 in x {
                let y0 = b0 * x0 + b2 * x2 - a1 * y1 - a2 * y2;
                (x2, x1, y2, y1) = (x1, x0, y1, y0);
                sum += f64::from(y0).powi(2);
            }
            sum
        };
        let tilt = |fc: f32| 10.0 * (band(&rl, fc) / band(&dl, fc)).log10() as f32;
        let row: Vec<String> = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0].iter().map(|&fc| format!("{fc}: {:+.1}", tilt(fc))).collect();
        println!("{name}: {}", row.join("  "));
        let bass = [125.0, 250.0, 500.0].map(tilt).into_iter().fold(f32::MIN, f32::max);
        assert!(bass - tilt(1000.0) < 6.0, "{name}: {}", row.join("  "));
    }
}

/// Speech-like noise: most of it 100 to 500 Hz, falling above.
fn speech(seed: &mut u32, state: &mut [f32; 2], out: &mut [f32]) {
    noise(seed, out);
    let (lp, hp) = (1.0 - (-2.0 * std::f32::consts::PI * 500.0 / 48_000.0).exp(), 1.0 - (-2.0 * std::f32::consts::PI * 100.0 / 48_000.0).exp());
    for x in out {
        state[0] += (*x - state[0]) * lp;
        state[1] += (state[0] - state[1]) * hp;
        *x = (state[0] - state[1]) * 3.0;
    }
}

/// A voice's rebounds against its direct sound at our ears (gain 1 each,
/// amplitude), and what `rebound_level` says.
fn at_the_ears(world: &Blocks, s: [f32; 3], l: [f32; 3]) -> (f32, f32) {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let e = Engine::new(Box::new(world.clone())).unwrap();
    let space = listener(l, NORTH);
    e.simulate(&space, &[(1, to_sa(s))], &QUALITY);
    let src = e.source(1).unwrap();
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    // Against the voice in plain sight.
    let open = vc_acoustics::sa::Direct { occlusion: 1.0, transmission: [1.0; 3] };
    let h = Heard { gain: 1.0, direct: open, dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
    let (mut seed, mut st, mut input) = (7u32, [0.0f32; 2], vec![0.0; 960]);
    let (mut sd, mut sr) = (0.0f64, 0.0f64);
    let e2 = |x: &[f32]| x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>();
    for k in 0..FRAMES {
        speech(&mut seed, &mut st, &mut input);
        let mut direct = vec![0.0f32; 1920];
        v.render(&e, Some(&src), &h, &input, &mut direct, &mut dec.ambi);
        let mut rebounds = vec![0.0f32; 1920];
        dec.decode(&e, &space, &mut rebounds);
        if k >= 20 {
            (sd, sr) = (sd + e2(&direct), sr + e2(&rebounds));
        }
    }
    ((sr / sd).sqrt() as f32, v.rebound_level())
}

/// `render::EAR` holds: what `rebound_level` says, at our ears.
#[test]
fn rebounds_at_the_ears() {
    for (name, w, s, l) in [
        ("diner 1 m", diner(false), [8.1, 4.0, 1.6], [8.1, 3.0, 1.6]),
        ("diner 3 m", diner(false), [8.1, 4.0, 1.6], [8.1, 1.0, 1.6]),
        ("diner door", diner(false), [8.1, 4.0, 1.6], [8.1, 0.0, 1.6]),
        ("diner hidden", diner(false), [3.0, 5.0, 1.6], [12.0, -2.0, 1.6]),
        ("room 3 m", room(), [1.0, 2.5, 1.6], [4.0, 2.5, 1.6]),
    ] {
        let (ear, level) = at_the_ears(&w, s, l);
        let over = 20.0 * (ear / level).log10();
        println!("{name:12}: level {:+5.1} dB, at the ears {:+5.1} dB, over {over:+4.1} dB", 20.0 * level.log10(), 20.0 * ear.log10());
        assert!((over - 20.0 * vc_acoustics::render::EAR.log10()).abs() < 2.0, "{name}: {over:+.1} dB");
    }
}

/// Speech's direct sound and rebounds at the ears (gain 1 each, energies,
/// the bass under 300 Hz left out as the ear does), and the rebounds' level.
fn weighed(world: &Blocks, s: [f32; 3], l: [f32; 3]) -> (f64, f64, f32) {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let e = Engine::new(Box::new(world.clone())).unwrap();
    let space = listener(l, NORTH);
    e.simulate(&space, &[(1, to_sa(s))], &QUALITY);
    let (d, src) = (e.direct_of(1).unwrap(), e.source(1).unwrap());
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    let h = Heard { gain: 1.0, direct: d, dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
    let (mut seed, mut st, mut input) = (7u32, [0.0f32; 2], vec![0.0; 960]);
    let a = 1.0 - (-2.0 * std::f32::consts::PI * 300.0 / 48_000.0).exp();
    let (mut lp, mut acc) = ([0.0f32; 4], [0.0f64; 2]);
    for k in 0..FRAMES {
        speech(&mut seed, &mut st, &mut input);
        let mut direct = vec![0.0f32; 1920];
        v.render(&e, Some(&src), &h, &input, &mut direct, &mut dec.ambi);
        let mut rebounds = vec![0.0f32; 1920];
        dec.decode(&e, &space, &mut rebounds);
        for (j, buf) in [&direct, &rebounds].into_iter().enumerate() {
            for x in buf.chunks_exact(2).map(|lr| lr[0]) {
                lp[2 * j] += (x - lp[2 * j]) * a;
                lp[2 * j + 1] += (lp[2 * j] - lp[2 * j + 1]) * a;
                if k >= 20 {
                    acc[j] += f64::from(x - lp[2 * j + 1]).powi(2);
                }
            }
        }
    }
    (acc[0], acc[1], v.rebound_level())
}

/// Walking from a placed voice to the diner's doorway 4 m away, with the
/// plugin's gains (audio.rs, a voice in sight): how loud at our ears (dB),
/// and the rebounds over the voice.
fn walk() -> Vec<(f32, f32, f32)> {
    let cap = 0.5 / vc_acoustics::render::EAR;
    let s = [8.1, 4.0, 1.6];
    [3.5f32, 3.0, 2.0, 1.0, 0.5, 0.0, -1.0, -3.0]
        .iter()
        .map(|&y| {
            let (d, r, level) = weighed(&diner(false), s, [8.1, y, 1.6]);
            let dist = 4.0 - y;
            let spread = 3.0 / dist.max(3.0);
            let raw_gain = 0.5 * (3.0 / dist.max(3.0)) * dist.max(1.0);
            let raw = raw_gain * level / spread;
            let rebounds = raw.min(cap);
            let rebound_gain = raw_gain * rebounds / raw;
            let factor = (1.0 + rebounds * rebounds).sqrt();
            let hold = if dist < 3.0 { (1.0 + (raw * 3.0 / dist.max(1.0)).min(cap).powi(2)).sqrt() / factor } else { 1.0 };
            let volume = hold;
            let (gd, gr) = (f64::from(spread * volume).powi(2), f64::from(rebound_gain * volume).powi(2));
            (dist, 10.0 * (d * gd + r * gr).log10() as f32, 10.0 * (r * gr / (d * gd)).log10() as f32)
        })
        .collect()
}

/// Walking away from a voice in plain sight it never gets louder, and its
/// rebounds stay under it (it swelled from 2 to 5 m, in game 2026-11-20).
#[test]
fn walking_away_it_only_gets_quieter() {
    let w = walk();
    for (dist, total, wet) in &w {
        println!("dist {dist:3.1}: {total:+6.1} dB, rebounds over the voice {wet:+5.1} dB");
    }
    assert!(w.iter().all(|(_, _, wet)| *wet < -4.0));
    // From the nearest to the farthest (the list runs 0.5 m to 7 m).
    assert!(w.windows(2).all(|p| p[1].1 <= p[0].1 + 0.3), "{w:?}");
}

/// Out of the diner's doorway and along the front, a voice inside 4 m from
/// the door, with the plugin's gains (audio.rs; the way round the door's
/// edge as CET rates it): how loud at our ears (dB). A step out of sight of
/// the door made it 7 dB louder, the rebounds uncapped there (in game 2026-11-20).
#[test]
fn round_the_corner_it_only_gets_quieter() {
    let cap = 0.5 / vc_acoustics::render::EAR;
    let (s, door) = ([8.1f32, 4.0, 1.6], [8.1f32, 0.0, 1.6]);
    let (open, _, _) = weighed(&diner(false), s, [8.1, 1.0, 1.6]);
    let mut last = f32::MAX;
    for l in [[8.1f32, -0.5, 1.6], [8.1, -1.5, 1.6], [7.0, -1.5, 1.6], [6.0, -1.5, 1.6], [5.0, -1.5, 1.6], [3.5, -1.5, 1.6], [2.0, -1.5, 1.6], [0.5, -1.5, 1.6], [-1.0, -1.5, 1.6]] {
        let d = {
            let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
            let e = Engine::new(Box::new(diner(false))).unwrap();
            e.simulate(&listener(l, NORTH), &[(1, to_sa(s))], &QUALITY);
            e.direct_of(1).unwrap()
        };
        let (direct, r, level) = weighed(&diner(false), s, l);
        let dist = ((s[0] - l[0]).powi(2) + (s[1] - l[1]).powi(2)).sqrt();
        let spread = 3.0 / dist.max(3.0);
        let raw_gain = 0.5 * (3.0 / dist.max(3.0)) * dist.max(1.0);
        let raw = raw_gain * level / spread;
        let rebound_gain = raw_gain * raw.min(cap) / raw;
        // Round the door's edge, out of its sight: 18 dB a half turn, over the way's length.
        let len = ((l[0] - door[0]).powi(2) + (l[1] - door[1]).powi(2)).sqrt() + 4.0;
        let turn = ((door[0] - l[0]) / (len - 4.0)).clamp(-1.0, 1.0).asin().abs().to_degrees();
        let bend = if d.occlusion > 0.5 { 0.0 } else { 10f32.powf(-(turn / 180.0) * 18.0 / 20.0) * (3.0 / len.max(3.0)) / spread };
        let total = 10.0
            * (direct * f64::from(spread).powi(2) + r * f64::from(rebound_gain).powi(2) + open * f64::from(spread * bend).powi(2)).log10() as f32;
        println!("at {:4.1} {:4.1}, in sight {:.2}: {total:+5.1} dB", l[0], l[1], d.occlusion);
        assert!(total <= last + 0.3, "louder at {l:?}: {total:+.1} dB after {last:+.1} dB");
        last = total;
    }
}


