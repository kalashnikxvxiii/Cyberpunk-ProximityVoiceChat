// The crate is named VoiceChat so the DLL is VoiceChat.dll.
#![allow(non_snake_case)]

mod api;
mod audio;
mod config;
mod coop;
mod events;
mod game;
mod gamemix;
mod i18n;
mod keys;
mod net;
mod rays;
mod session;
mod sound3d;
mod state;

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use red4ext_rs::{
    Exportable, GameApp, Plugin, PluginOps, SdkEnv, SemVer, StateHandlerResult, StateListener,
    StateType, U16CStr, export_plugin_symbols, log, wcstr,
};

use crate::net::ChannelSlot;
use crate::state::Shared;

pub struct VoiceChat;

impl Plugin for VoiceChat {
    const AUTHOR: &'static U16CStr = wcstr!("kalashnikxv");
    const NAME: &'static U16CStr = wcstr!("VoiceChat");
    const VERSION: SemVer = SemVer::new(0, 2, 0);

    fn exports() -> impl Exportable {
        api::exports()
    }

    fn on_load(env: &SdkEnv) {
        log::info!(env, "VoiceChat {} loaded", env!("CARGO_PKG_VERSION"));
        // Before the game opens its audio stream, so we learn its format; on a thread of ours (COM).
        let hooked = thread::spawn(gamemix::install).join().unwrap_or_else(|_| Err("panicked".into()));
        match hooked {
            Ok(()) => log::info!(env, "audio: listening to the game's sound"),
            Err(e) => log::info!(env, "audio: can't listen to the game's sound ({e}): the place is read from raycasts only"),
        }
        env.add_listener(
            StateType::Running,
            StateListener::default()
                .with_on_enter(on_running)
                .with_on_update(on_update),
        );
    }

    fn on_unload(_env: &SdkEnv) {
        coop::stop();
        if let Some(engine) = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take() {
            engine.shared.stop.store(true, Ordering::Relaxed);
            sound3d::close(&engine.shared);
            for t in engine.threads {
                let _ = t.join();
            }
        }
        gamemix::uninstall();
        // A game audio thread may be inside a hook right now: let it leave before our code goes.
        thread::sleep(Duration::from_millis(100));
    }
}

export_plugin_symbols!(VoiceChat);

/// Logs through RED4ext from any thread.
pub(crate) fn log_info(args: fmt::Arguments<'_>) {
    log::info!(VoiceChat::env(), "{args}");
}

struct Engine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

fn start() -> Engine {
    let settings = config::load();
    let shared = Arc::new(Shared::new(settings));
    let slot = Arc::new(ChannelSlot::default());
    let spawn = |name: &str, f: Box<dyn FnOnce() + Send>| thread::Builder::new().name(name.into()).spawn(f).ok();
    let threads = [
        spawn("voicechat-session", Box::new({
            let (shared, slot) = (shared.clone(), slot.clone());
            move || session::run(shared, slot)
        })),
        spawn("voicechat-audio", Box::new({
            let shared = shared.clone();
            move || audio::run(shared, slot)
        })),
        sound3d::start(&shared),
    ];
    Engine { shared, threads: threads.into_iter().flatten().collect() }
}

pub(crate) fn shared() -> Option<Arc<Shared>> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|e| e.shared.clone())
}

unsafe extern "C" fn on_running(_app: &GameApp) -> StateHandlerResult {
    let mut engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if engine.is_none() {
        *engine = Some(start());
    }
    StateHandlerResult::Finished
}

const POSE_INTERVAL: Duration = Duration::from_millis(50);

thread_local! {
    static LAST_POSE: Cell<Option<Instant>> = const { Cell::new(None) };
    static LAST_ERRORS: RefCell<Vec<(&'static str, String)>> = const { RefCell::new(Vec::new()) };
}

/// Logs a per-frame error only when it differs from the last one for `what`.
pub(crate) fn log_once(what: &'static str, err: &dyn fmt::Display) {
    let msg = err.to_string();
    LAST_ERRORS.with_borrow_mut(|errs| match errs.iter_mut().find(|(w, _)| *w == what) {
        Some((_, last)) if *last == msg => {}
        Some((_, last)) => {
            log_info(format_args!("{what}: {msg}"));
            *last = msg;
        }
        None => {
            log_info(format_args!("{what}: {msg}"));
            errs.push((what, msg));
        }
    });
}

unsafe extern "C" fn on_update(_app: &GameApp) -> StateHandlerResult {
    keys::poll_capture();
    if let Some(shared) = shared() {
        coop::tick(&shared);
        // Steam Audio's rays, every frame (they wait for us).
        sound3d::tick(&shared);
    }
    let now = Instant::now();
    if LAST_POSE.get().is_some_and(|t| now - t < POSE_INTERVAL) {
        return StateHandlerResult::Running;
    }
    LAST_POSE.set(Some(now));
    let Some(shared) = shared() else {
        return StateHandlerResult::Running;
    };
    let pose = game::player_pose()
        .map_err(|e| log_once("player_pose", &e))
        .ok()
        .flatten()
        .map(|mut p| {
            // Prefer the camera: the body keeps facing the car's bonnet.
            match game::camera_forward() {
                Ok(f) if f.x.abs() + f.y.abs() > 1e-3 => p.forward = f,
                Ok(_) => {}
                Err(e) => log_once("camera_forward", &e),
            }
            p
        });
    *shared.lock(&shared.pose) = pose;
    StateHandlerResult::Running
}
