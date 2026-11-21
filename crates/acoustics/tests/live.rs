//! As in the game: rebounds rerun on their own thread over and over, the
//! direct sound on another, while the audio thread renders all along.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vc_acoustics::materials::CONCRETE;
use vc_acoustics::render::{Decoder, Heard, Voice, direction};
use vc_acoustics::world::{Blocks, block, listener, to_sa};
use vc_acoustics::{Engine, QUALITY};

fn room() -> Blocks {
    let t = 0.3;
    Blocks(vec![
        block([-40.0, -40.0, -1.0], [50.0, 60.0, 0.0], CONCRETE),
        block([-t, -t, 0.0], [0.0, 5.0 + t, 3.0], CONCRETE),
        block([6.0, -t, 0.0], [6.0 + t, 5.0 + t, 3.0], CONCRETE),
        block([-t, -t, 0.0], [6.0 + t, 0.0, 3.0], CONCRETE),
        block([-t, 5.0, 0.0], [6.0 + t, 5.0 + t, 3.0], CONCRETE),
        block([-t, -t, 3.0], [6.0 + t, 5.0 + t, 3.0 + t], CONCRETE),
    ])
}

#[test]
fn rebounds_keep_ringing_while_the_simulation_reruns() {
    let e = Engine::new(Box::new(room())).unwrap();
    let (s, l) = ([2.0, 2.0, 1.6], [5.0, 4.0, 1.6]);
    let space = listener(l, [0.0, 1.0, 0.0]);
    let voices = vec![(1u32, to_sa(s))];
    e.sync(&[1]);
    e.direct(&space, &voices, &QUALITY);
    let stop = Arc::new(AtomicBool::new(false));
    let sim = {
        let (e, stop, voices) = (e.clone(), stop.clone(), voices.clone());
        std::thread::spawn(move || {
            let space = listener(l, [0.0, 1.0, 0.0]);
            let mut runs = 0;
            while !stop.load(Ordering::Relaxed) {
                e.sync(&[1]);
                e.rebounds(&space, &voices, &QUALITY);
                runs += 1;
            }
            runs
        })
    };
    // The direct sound on a thread of its own, at the same time (the game thread).
    let direct = {
        let (e, stop, voices) = (e.clone(), stop.clone(), voices.clone());
        std::thread::spawn(move || {
            let space = listener(l, [0.0, 1.0, 0.0]);
            while !stop.load(Ordering::Relaxed) {
                e.direct(&space, &voices, &QUALITY);
                std::thread::sleep(Duration::from_millis(3));
            }
        })
    };
    let mut v = Voice::new(&e).unwrap();
    let mut dec = Decoder::new(&e).unwrap();
    let h = Heard { gain: 1.0, direct: e.direct_of(1).unwrap(), dir: direction(&e, to_sa(s), &space), air: [1.0; 3], rebounds: 1.0, bends: [None; 2] };
    let mut seed = 7u32;
    let mut input = vec![0.0f32; 960];
    let start = Instant::now();
    let mut levels = Vec::new();
    while start.elapsed() < Duration::from_secs(4) {
        for x in &mut input {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *x = ((seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.2;
        }
        let src = e.source(1);
        let mut mix = vec![0.0f32; 1920];
        v.render(&e, src.as_deref(), &h, &input, &mut mix, &mut dec.ambi);
        dec.decode(&e, &space, &mut mix);
        levels.push(v.rebound_level());
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    let runs = sim.join().unwrap();
    direct.join().unwrap();
    let tail = &levels[levels.len() / 2..];
    let min = tail.iter().cloned().fold(f32::MAX, f32::min);
    println!("runs {runs}, frames {}, level min {min:.3} last {:.3}", levels.len(), levels.last().unwrap());
    assert!(runs > 2);
    assert!(min > 0.01, "rebounds died: {:?}", &levels[levels.len() - 20..]);
}
