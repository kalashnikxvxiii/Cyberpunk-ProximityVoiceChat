//! The simulation: where each voice can be heard from, through what, and how
//! the places around ring.
//!
//! Two kinds, on two threads, as Steam Audio allows:
//! - [`Engine::direct`] (what hides each voice, what it goes through): few
//!   single rays, asked one after the other; run where rays are answered at
//!   once (the game thread).
//! - [`Engine::rebounds`]: many rays in big batches; run on a thread of its
//!   own (it waits while the game answers them, a frame at a time).
//!
//! [`Engine::sync`] adds and removes voices between rebound runs. The audio
//! thread reads results through [`Engine::source`] and [`Engine::direct_of`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::sa::{self, Context, DIRECT, Direct, Hrtf, Quality, REFLECTIONS, Scene, Simulator, Source, Space, V3};

/// A voice is a sphere this big, not a point (see `Source::set`).
pub const VOICE_RADIUS: f32 = 0.25;

// Fields drop top to bottom: what uses the context goes first, the context last.
pub struct Engine {
    sources: Mutex<HashMap<u32, Arc<Source>>>,
    direct: Mutex<HashMap<u32, Direct>>,
    /// Held while sources change and while the direct sound runs: never
    /// both at once. (Rebounds run outside it, on the thread that changes sources.)
    changing: Mutex<()>,
    sim: Simulator,
    // Kept alive as long as the simulator uses it.
    _scene: Scene,
    pub hrtf: Hrtf,
    pub ctx: Context,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Engine {
    pub fn new(tracer: Box<dyn sa::Tracer>) -> Result<Arc<Self>, String> {
        let ctx = Context::new()?;
        let hrtf = Hrtf::new(&ctx)?;
        let scene = Scene::new(&ctx, tracer)?;
        let sim = Simulator::new(&ctx, &scene)?;
        Ok(Arc::new(Self {
            sources: Mutex::default(),
            direct: Mutex::default(),
            changing: Mutex::new(()),
            sim,
            _scene: scene,
            hrtf,
            ctx,
        }))
    }

    /// The voices to simulate: adds new ones, drops the gone. Call between
    /// rebound runs (on their thread).
    pub fn sync(&self, ids: &[u32]) {
        // Same order as `direct`: `changing`, then `sources`.
        let _changing = lock(&self.changing);
        let mut sources = lock(&self.sources);
        let gone: Vec<u32> = sources.keys().filter(|id| !ids.contains(id)).copied().collect();
        let new: Vec<u32> = ids.iter().filter(|id| !sources.contains_key(id)).copied().collect();
        if gone.is_empty() && new.is_empty() {
            return;
        }
        for id in gone {
            if let Some(s) = sources.remove(&id) {
                s.remove(&self.sim);
            }
            lock(&self.direct).remove(&id);
        }
        for id in new {
            if sources.len() >= sa::MAX_SOURCES as usize {
                break;
            }
            if let Ok(s) = Source::new(&self.sim) {
                sources.insert(id, Arc::new(s));
            }
        }
        self.sim.commit();
    }

    /// What hides each voice and what it goes through, for voices already
    /// added by [`Engine::sync`]. Rays are asked one by one: call where they
    /// are answered at once.
    pub fn direct(&self, listener: &Space, voices: &[(u32, V3)], q: &Quality) {
        let _changing = lock(&self.changing);
        let mut ran: Vec<(u32, Arc<Source>)> = Vec::new();
        {
            let all = lock(&self.sources);
            for (id, at) in voices {
                if let Some(s) = all.get(id) {
                    s.set(DIRECT, *at, q, VOICE_RADIUS);
                    ran.push((*id, s.clone()));
                }
            }
        }
        if ran.is_empty() {
            return;
        }
        self.sim.set_listener(DIRECT, listener, q);
        self.sim.run_direct();
        let mut direct = lock(&self.direct);
        for (id, s) in ran {
            direct.insert(id, s.direct());
        }
    }

    /// The rebounds for every voice (many rays): on a thread of its own.
    pub fn rebounds(&self, listener: &Space, voices: &[(u32, V3)], q: &Quality) {
        {
            let all = lock(&self.sources);
            if all.is_empty() {
                return;
            }
            for (id, at) in voices {
                if let Some(s) = all.get(id) {
                    s.set(REFLECTIONS, *at, q, VOICE_RADIUS);
                }
            }
        }
        self.sim.set_listener(REFLECTIONS, listener, q);
        self.sim.run_reflections();
    }

    /// All three, one after the other (tests, or a tracer that answers at once).
    pub fn simulate(&self, listener: &Space, voices: &[(u32, V3)], q: &Quality) {
        let ids: Vec<u32> = voices.iter().map(|(id, _)| *id).collect();
        self.sync(&ids);
        self.direct(listener, voices, q);
        self.rebounds(listener, voices, q);
    }

    pub fn source(&self, id: u32) -> Option<Arc<Source>> {
        lock(&self.sources).get(&id).cloned()
    }

    pub fn direct_of(&self, id: u32) -> Option<Direct> {
        lock(&self.direct).get(&id).copied()
    }
}
