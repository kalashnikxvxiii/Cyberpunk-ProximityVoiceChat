//! The game's physics raycasts, for Steam Audio (see `sound3d`).
//!
//! Steam Audio asks for rays from its own threads, but the game's physics
//! must be asked from the game thread. So rays wait in a [`Service`]: the
//! simulation posts a batch and sleeps; the game thread answers some every
//! frame (within [`BUDGET`]) and wakes it when the batch is done.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use red4ext_rs::types::{CName, GameInstance, Ref};
use red4ext_rs::{NativeRepr, ScriptClass, call, class_kind};
use vc_acoustics::materials;

use crate::game::Vector4;

pub struct SpatialQueriesSystem;

unsafe impl ScriptClass for SpatialQueriesSystem {
    type Kind = class_kind::Scripted;
    const NAME: &'static str = "gameSpatialQueriesSystem";
}

/// `physics::TraceResult` (RED4ext.SDK physicsTraceResult.hpp: 0x60 bytes).
#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct TraceResult {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub material: u64,
    _transform: [u8; 0x20],
    _unk40: u64,
    _proxy_id: u32,
    _actor_index: u32,
    _shape_index: u32,
    pub distance: f32,
    _rest: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<TraceResult>() == 0x60);

impl Default for TraceResult {
    fn default() -> Self {
        // Plain bytes: all zeros is a valid value.
        unsafe { std::mem::zeroed() }
    }
}

/// The `out result` argument: the engine writes the result into the first
/// 0x60 bytes, where the call's argument storage lives; when the call drops
/// its arguments we copy them out to `sink`. (Checked in game 2026-11-20:
/// the floor under the player came back at ear height, "concrete.physmat".)
#[repr(C, align(16))]
struct Out {
    result: TraceResult,
    sink: *mut TraceResult,
}

unsafe impl NativeRepr for Out {
    const NAME: &'static str = "physicsTraceResult";
}

impl Drop for Out {
    fn drop(&mut self) {
        if !self.sink.is_null() {
            unsafe { *self.sink = self.result };
        }
    }
}

/// The game's spatial queries, while it has a physics world to ask. Loading
/// a save tears the world down under the system, which stays: a raycast then
/// read through its null world and the game crashed (2026-11-20, three times,
/// right after "OnAfterWorldDetach"). Its raycast reads system+0x40, then
/// +0x78, then +0x60 (Cyberpunk2077.exe 2.50, +0x117bc20): both must be there.
fn system() -> Option<Ref<SpatialQueriesSystem>> {
    let sys = call!("ScriptGameInstance"::"GetSpatialQueriesSystem" (GameInstance::new()) -> Ref<SpatialQueriesSystem>).ok()?;
    has_world(&sys).then_some(sys)
}

fn has_world(sys: &Ref<SpatialQueriesSystem>) -> bool {
    let Some(this) = (unsafe { sys.instance() }) else { return false };
    let at = |p: *const u8, off: usize| unsafe { (p.add(off) as *const *const u8).read() };
    let owner = at(this as *const _ as *const u8, 0x40);
    !owner.is_null() && !at(owner, 0x78).is_null()
}

/// One ray, start -> end, against one collision group. `None`: nothing hit.
fn raycast(sys: &Ref<SpatialQueriesSystem>, start: Vector4, end: Vector4, group: CName) -> Option<TraceResult> {
    let mut got = TraceResult::default();
    let out = Out { result: TraceResult::default(), sink: &mut got };
    let hit = call!(sys, "SyncRaycastByCollisionGroup" (start, end, group, out, false, false) -> bool).ok()?;
    hit.then_some(got)
}

/// What stops sound: the world, glass and props, closed doors, cars.
const GROUPS: [&str; 5] = ["Static", "Terrain", "Destructible", "Dynamic", "Vehicle"];
/// Longest ray asked of the game: farther counts as open sky.
const FAR: f32 = 80.0;
/// Game-thread time spent on rays per frame.
/// (1 ms gave a rebound update every 1.1 s, 2 ms every 0.4-1 s, neither
/// changed the FPS; walking out of a doorway still lagged, 2026-11-20.)
pub const BUDGET: Duration = Duration::from_micros(3000);
/// A batch not done by then (the game stopped updating: loading) is given up.
const PATIENCE: Duration = Duration::from_secs(2);

/// A ray in game axes: from `origin` along unit `dir`, between `min` and `max`.
#[derive(Debug, Clone, Copy)]
pub struct GameRay {
    pub origin: [f32; 3],
    pub dir: [f32; 3],
    pub min: f32,
    pub max: f32,
    /// Goes through a leaky surface letting more than this share by
    /// (`sa::ray_hash`: the same rays every time).
    pub pass: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct GameHit {
    pub distance: f32,
    pub normal: [f32; 3],
    pub material: u8,
}

struct Job {
    rays: Vec<GameRay>,
    hits: Vec<Option<GameHit>>,
    next: usize,
}

impl Stats {
    fn slice(&mut self, took: Duration) {
        self.busy += took;
        if self.longest.is_none_or(|(l, _)| took > l) {
            self.longest = Some((took, Instant::now()));
        }
    }
}

/// Counters for the diagnostics.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub rays: u64,
    pub busy: Duration,
    /// The longest the game thread spent on rays at once, and when: a frame
    /// the game froze for (in game 2026-11-21) is ours or not.
    pub longest: Option<(Duration, Instant)>,
    pub given_up: u64,
    /// Hits whose own distance field disagreed with where they hit.
    pub distance_off: u64,
    /// Game material names met, and the material they count as.
    pub materials: std::collections::BTreeMap<String, &'static str>,
}

thread_local! {
    static GAME_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Whether this is the game thread (it runs [`Service::run`]).
pub fn on_game_thread() -> bool {
    GAME_THREAD.get()
}

#[derive(Default)]
pub struct Service {
    /// The plugin is closing: nobody will answer rays any more.
    closing: AtomicBool,
    job: Mutex<Option<Job>>,
    done: Condvar,
    pub stats: Mutex<Stats>,
    /// Game material name hash -> ours.
    names: Mutex<HashMap<u64, u8>>,
    last_run: Mutex<Option<Instant>>,
}

impl Service {
    /// From the simulation thread: traces `rays` on the game thread, waiting.
    /// `None`: given up (see [`PATIENCE`]).
    pub fn trace(&self, rays: Vec<GameRay>) -> Option<Vec<Option<GameHit>>> {
        if self.closing.load(Ordering::Relaxed) || !self.alive() {
            return None;
        }
        let n = rays.len();
        let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        *job = Some(Job { rays, hits: vec![None; n], next: 0 });
        let deadline = Instant::now() + PATIENCE;
        loop {
            if job.as_ref().is_some_and(|j| j.next >= n) {
                return job.take().map(|j| j.hits);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.closing.load(Ordering::Relaxed) {
                *job = None;
                self.stats.lock().unwrap_or_else(|e| e.into_inner()).given_up += 1;
                return None;
            }
            job = self.done.wait_timeout(job, left.min(Duration::from_millis(100))).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Whether the game thread answered rays recently (in the world, not loading).
    pub fn alive(&self) -> bool {
        self.last_run.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < Duration::from_millis(500))
    }

    /// Stops answering: waiting simulations give up at once.
    pub fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.done.notify_all();
    }

    /// On the game thread: answers `rays` now (for the direct sound).
    pub fn trace_now(&self, rays: &[GameRay]) -> Vec<Option<GameHit>> {
        let Some(sys) = system() else { return vec![None; rays.len()] };
        let groups: Vec<CName> = GROUPS.iter().map(|g| CName::new(g)).collect();
        let t = Instant::now();
        let hits = rays.iter().map(|r| self.one(&sys, &groups, r)).collect();
        let mut st = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        st.rays += rays.len() as u64;
        st.slice(t.elapsed());
        hits
    }

    /// On the game thread, every frame: answers waiting rays for up to [`BUDGET`].
    pub fn run(&self) {
        GAME_THREAD.set(true);
        *self.last_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let mut guard = self.job.lock().unwrap_or_else(|e| e.into_inner());
        let Some(job) = guard.as_mut() else { return };
        if job.next >= job.rays.len() {
            return;
        }
        let Some(sys) = system() else { return };
        let groups: Vec<CName> = GROUPS.iter().map(|g| CName::new(g)).collect();
        let t = Instant::now();
        let start = job.next;
        while job.next < job.rays.len() && t.elapsed() < BUDGET {
            let r = job.rays[job.next];
            job.hits[job.next] = self.one(&sys, &groups, &r);
            job.next += 1;
        }
        let done = job.next >= job.rays.len();
        {
            let mut st = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            st.rays += (job.next - start) as u64;
            st.slice(t.elapsed());
        }
        drop(guard);
        if done {
            self.done.notify_all();
        }
    }

    /// The ray's nearest hit; through what lets some sound by (a door's
    /// gaps, a grate's holes) it goes on if its share says so (`pass`), here
    /// in the same frame: each such pass used to cost Steam Audio another
    /// round trip to the game, a frame each (2026-11-20).
    fn one(&self, sys: &Ref<SpatialQueriesSystem>, groups: &[CName], r: &GameRay) -> Option<GameHit> {
        let mut start = r.min;
        for _ in 0..4 {
            let hit = self.nearest(sys, groups, r, start)?;
            if materials::leak(materials::ptr(hit.material)) <= r.pass {
                return Some(hit);
            }
            start = hit.distance + 0.02;
        }
        None
    }

    fn nearest(&self, sys: &Ref<SpatialQueriesSystem>, groups: &[CName], r: &GameRay, start: f32) -> Option<GameHit> {
        let max = r.max.min(FAR);
        if !(max > start) {
            return None;
        }
        let at = |d: f32| Vector4 { x: r.origin[0] + r.dir[0] * d, y: r.origin[1] + r.dir[1] * d, z: r.origin[2] + r.dir[2] * d, w: 1.0 };
        let from = at(start);
        let mut best: Option<(TraceResult, f32, usize)> = None;
        let mut off = 0u64;
        for (k, g) in groups.iter().enumerate() {
            // Only up to the nearest hit so far: shorter rays, same answer.
            let reach = best.map_or(max - start, |(_, d, _)| d);
            let Some(h) = raycast(sys, from, at(start + reach), *g) else { continue };
            // The distance from where it hit, as CET's acoustics do: the
            // result's own field isn't to be trusted, and a hit at (0,0,0)
            // is no hit.
            let p = h.position;
            let d = ((p[0] - from.x).powi(2) + (p[1] - from.y).powi(2) + (p[2] - from.z).powi(2)).sqrt();
            if p == [0.0; 3] || !d.is_finite() || d < 0.002 || d > reach + 0.01 {
                continue;
            }
            if (h.distance - d).abs() > 0.05 {
                off += 1;
            }
            best = Some((h, d.min(reach), k));
        }
        if off > 0 {
            self.stats.lock().unwrap_or_else(|e| e.into_inner()).distance_off += off;
        }
        let (h, d, k) = best?;
        let mut material = self.material(h.material);
        // A closed door is Dynamic (it moves); so are props: muffle like a door.
        if GROUPS[k] == "Dynamic" && material != materials::GLASS && material != materials::SOFT {
            material = materials::DOOR;
        }
        // Steam Audio shades rebounds by the normal: a zero one would silence them.
        let len = h.normal.iter().map(|v| v * v).sum::<f32>().sqrt();
        let normal = if (0.5..1.5).contains(&len) { h.normal } else { r.dir.map(|v| -v) };
        Some(GameHit { distance: start + d, normal, material })
    }

    fn material(&self, hash: u64) -> u8 {
        let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = names.get(&hash) {
            return *m;
        }
        let name = CName::from(hash).as_str().to_owned();
        let m = materials::classify(&name);
        names.insert(hash, m);
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).materials.insert(name, materials::NAMES[usize::from(m)]);
        m
    }
}
