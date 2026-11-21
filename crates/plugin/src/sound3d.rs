//! Steam Audio in the game: loads phonon.dll, answers its rays with the
//! game's physics (through `rays::Service`), and runs the simulation on a
//! thread of its own. The audio thread renders with the results.
//!
//! If phonon.dll is missing or won't start, nothing here runs and the audio
//! thread keeps the classic acoustics (CET's paths): the mod still works.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vc_acoustics::sa::{self, Hit, Ray};
use vc_acoustics::world::{to_game, to_sa};
use vc_acoustics::{Engine, QUALITY, materials, world};
use vc_dsp::spatial;

use crate::rays::{GameRay, Service};
use crate::state::Shared;

/// Steam Audio, once it started.
pub struct Sound3d {
    pub engine: Arc<Engine>,
    pub service: Arc<Service>,
    /// Last simulation: when, how long it took.
    pub last: Mutex<Option<(Instant, Duration)>>,
}

/// Voices farther than this aren't simulated (silent anyway).
const MAX_DISTANCE: f32 = 250.0;
/// The direct sound (what hides a voice) this often, on the game thread;
/// the rebounds as often as their rays come back, at most this often too.
const EVERY: Duration = Duration::from_millis(100);

struct GameTracer(Arc<Service>);

impl sa::Tracer for GameTracer {
    fn leaks_inside(&self) -> bool {
        true
    }

    fn closest(&self, rays: &[Ray], min: &[f32], max: &[f32], hits: &mut [Hit]) {
        let batch: Vec<GameRay> = rays
            .iter()
            .zip(min)
            .zip(max)
            .map(|((r, &min), &max)| GameRay { origin: to_game(r.origin), dir: to_game(r.direction), min, max, pass: sa::ray_hash(r) })
            .collect();
        // On the game thread (the direct sound): at once. Else wait for it.
        let found = if crate::rays::on_game_thread() { Some(self.0.trace_now(&batch)) } else { self.0.trace(batch) };
        let Some(found) = found else {
            // Given up (loading, closing): everything blocked, right where the
            // rays start. As misses, every rebound would see every voice: one
            // run far too loud. Blocked, it's one run too quiet; the next fixes it.
            for ((h, r), &min) in hits.iter_mut().zip(rays).zip(min) {
                let d = r.direction;
                *h = Hit {
                    distance: min,
                    triangleIndex: -1,
                    objectIndex: -1,
                    materialIndex: -1,
                    normal: sa::v3(-d.x, -d.y, -d.z),
                    material: materials::ptr(materials::GENERIC),
                };
            }
            return;
        };
        for (i, h) in hits.iter_mut().enumerate() {
            *h = match found.get(i).copied().flatten() {
                Some(g) => Hit {
                    distance: g.distance,
                    triangleIndex: -1,
                    objectIndex: -1,
                    materialIndex: -1,
                    normal: to_sa(g.normal),
                    material: materials::ptr(g.material),
                },
                None => sa::miss(),
            };
        }
    }
}

/// Loads phonon.dll from our plugin folder (the game wouldn't look there).
#[cfg(windows)]
fn load_phonon() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
    let path = crate::config::path().with_file_name("phonon.dll");
    if !path.exists() {
        return Err(format!("{} not found", path.display()));
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let module = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
    if module.is_null() { Err(format!("can't load {}", path.display())) } else { Ok(()) }
}

#[cfg(not(windows))]
fn load_phonon() -> Result<(), String> {
    Ok(())
}

/// Starts Steam Audio and its simulation thread, or logs why not.
pub fn start(shared: &Arc<Shared>) -> Option<JoinHandle<()>> {
    let started = load_phonon().and_then(|()| {
        let service = Arc::new(Service::default());
        let engine = Engine::new(Box::new(GameTracer(service.clone())))?;
        Ok(Sound3d { engine, service, last: Mutex::new(None) })
    });
    match started {
        Ok(s) => {
            crate::log_info(format_args!("acoustics: Steam Audio ready"));
            let _ = shared.sound3d.set(s);
        }
        Err(e) => {
            crate::log_info(format_args!("acoustics: Steam Audio unavailable ({e}), classic acoustics"));
            return None;
        }
    }
    let shared = shared.clone();
    thread::Builder::new().name("voicechat-acoustics".into()).spawn(move || run(&shared)).ok()
}

/// Steam Audio is on: started, and wanted.
pub fn active(shared: &Shared) -> Option<&Sound3d> {
    let s = shared.settings();
    shared.sound3d.get().filter(|_| s.occlusion && s.steam_audio)
}

fn run(shared: &Shared) {
    while !shared.stop.load(Ordering::Relaxed) {
        let t = Instant::now();
        rebounds(shared);
        thread::sleep(EVERY.saturating_sub(t.elapsed()).max(Duration::from_millis(10)));
    }
}

/// The listener (ears, view) and the voices to simulate, game axes turned
/// into Steam Audio's.
fn scene(shared: &Shared) -> Option<(sa::Space, Vec<(u32, sa::V3)>)> {
    let pose = (*shared.lock(&shared.pose))?;
    let ear = spatial::ear(pose.pos(), shared.ear());
    let voices: Vec<(u32, sa::V3)> = shared
        .lock(&shared.speakers)
        .iter()
        .filter(|(_, i)| i.last_packet.elapsed() < Duration::from_secs(10))
        .filter_map(|(id, i)| {
            let at = spatial::ear(i.pos?, i.env.ear_m().unwrap_or(spatial::EAR_HEIGHT));
            (spatial::distance(at, ear) < MAX_DISTANCE).then(|| (u32::from(*id), to_sa(at)))
        })
        .collect();
    Some((world::listener(ear, pose.forward_vec()), voices))
}

/// On the simulation thread: voices in and out, then their rebounds.
fn rebounds(shared: &Shared) {
    let Some(s3d) = active(shared) else { return };
    // In the world, the game answering rays.
    if !s3d.service.alive() {
        return;
    }
    let Some((listener, voices)) = scene(shared) else { return };
    let ids: Vec<u32> = voices.iter().map(|(id, _)| *id).collect();
    s3d.engine.sync(&ids);
    if voices.is_empty() {
        return;
    }
    // Anyone talking where it rings all round (a tunnel): more bounces.
    let ringing = shared.lock(&shared.speakers).values().any(|i| crate::audio::ring_of(f32::from(i.env.wet) / 255.0, f32::from(i.env.size) / 255.0) > 0.5);
    let t = Instant::now();
    s3d.engine.rebounds(&listener, &voices, if ringing { &vc_acoustics::RINGING } else { &QUALITY });
    *s3d.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), t.elapsed()));
}

thread_local! {
    static LAST_DIRECT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// On the game thread, every frame: answers the rebounds' rays, and every
/// [`EVERY`] works out the direct sound (its rays answered at once).
pub fn tick(shared: &Shared) {
    let Some(s3d) = shared.sound3d.get() else { return };
    s3d.service.run();
    if active(shared).is_none() || LAST_DIRECT.get().is_some_and(|t| t.elapsed() < EVERY) {
        return;
    }
    LAST_DIRECT.set(Some(Instant::now()));
    if let Some((listener, voices)) = scene(shared) {
        if !voices.is_empty() {
            s3d.engine.direct(&listener, &voices, &QUALITY);
        }
    }
}

/// Stops the simulation waiting for rays (the plugin is closing).
pub fn close(shared: &Shared) {
    if let Some(s3d) = shared.sound3d.get() {
        s3d.service.close();
    }
}
