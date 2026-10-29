// The crate is named VoiceChat so the DLL is VoiceChat.dll.
#![allow(non_snake_case)]

mod audio;
mod config;
mod game;
mod net;

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use red4ext_rs::{
    Exportable, GameApp, GlobalExport, Plugin, PluginOps, SdkEnv, SemVer, StateHandlerResult,
    StateListener, StateType, U16CStr, export_plugin_symbols, exports, global, log, wcstr,
};

use crate::net::{Net, Shared};

pub struct VoiceChat;

impl Plugin for VoiceChat {
    const AUTHOR: &'static U16CStr = wcstr!("kalashnikxv");
    const NAME: &'static U16CStr = wcstr!("VoiceChat");
    const VERSION: SemVer = SemVer::new(0, 1, 1);

    fn exports() -> impl Exportable {
        exports![
            GlobalExport(global!(c"VoiceChat_JoinParty", join_party)),
            GlobalExport(global!(c"VoiceChat_LeaveParty", leave_party)),
        ]
    }

    fn on_load(env: &SdkEnv) {
        log::info!(env, "VoiceChat {} loaded", env!("CARGO_PKG_VERSION"));
        env.add_listener(
            StateType::Running,
            StateListener::default()
                .with_on_enter(on_running)
                .with_on_update(on_update),
        );
    }

    fn on_unload(_env: &SdkEnv) {
        if let Some(engine) = ENGINE.lock().unwrap().take() {
            engine.shared.stop.store(true, Ordering::Relaxed);
            for t in engine.threads {
                let _ = t.join();
            }
        }
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

fn start() -> Option<Engine> {
    let cfg = config::load();
    log_info(format_args!(
        "config: server {} room '{}' name '{}' party '{}' ptt 0x{:02X}",
        cfg.server, cfg.room, cfg.name, cfg.party, cfg.ptt_vk
    ));
    if cfg.server.is_empty() {
        log_info(format_args!("config: no server set in config.ini, voice chat disabled"));
        return None;
    }
    let net = Net::connect(&cfg.server)
        .map_err(|e| log_info(format_args!("net: {e}")))
        .ok()?;
    let tx = net.sender()?;
    let shared = Arc::new(Shared::new(cfg));
    let spawn = |name: &str, f: Box<dyn FnOnce() + Send>| thread::Builder::new().name(name.into()).spawn(f).ok();
    let threads = [
        spawn("voicechat-net", Box::new({
            let shared = shared.clone();
            move || net.run(shared)
        })),
        spawn("voicechat-audio", Box::new({
            let shared = shared.clone();
            move || audio::run(shared, tx)
        })),
    ];
    Some(Engine { shared, threads: threads.into_iter().flatten().collect() })
}

fn shared() -> Option<Arc<Shared>> {
    ENGINE.lock().unwrap().as_ref().map(|e| e.shared.clone())
}

fn join_party(name: String) {
    let name = name.trim().to_owned();
    if let Some(s) = shared() {
        log_info(format_args!("party: join '{name}'"));
        *s.party.lock().unwrap() = (!name.is_empty()).then_some(name);
    }
}

fn leave_party() {
    if let Some(s) = shared() {
        log_info(format_args!("party: leave"));
        *s.party.lock().unwrap() = None;
    }
}

unsafe extern "C" fn on_running(_app: &GameApp) -> StateHandlerResult {
    let mut engine = ENGINE.lock().unwrap();
    if engine.is_none() {
        *engine = start();
    }
    StateHandlerResult::Finished
}

const POSE_INTERVAL: Duration = Duration::from_millis(50);

thread_local! {
    static LAST_POSE: Cell<Option<Instant>> = const { Cell::new(None) };
    static LAST_ERRORS: RefCell<Vec<(&'static str, String)>> = const { RefCell::new(Vec::new()) };
}

/// Logs a per-frame error only when it differs from the last one for `what`.
fn log_once(what: &'static str, err: &dyn fmt::Display) {
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
    *shared.pose.lock().unwrap() = pose;
    StateHandlerResult::Running
}
