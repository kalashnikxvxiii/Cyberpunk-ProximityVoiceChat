//! Decides how to connect and keeps it that way, following NightCoop and the
//! settings:
//!
//! - **NightCoop** (host or guest of a session): the voice goes through
//!   NightCoop's channels, run from the game thread ([`crate::coop`]); nothing
//!   to do here but say so;
//! - **manual**: the server and room from the settings;
//! - **offline**: nothing to do (and why).
//!
//! Every step reports to the diagnostics register, with what to do about it.

use crate::tr;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vc_rendezvous::secret::Secret;

use crate::config::{self, Settings};
use crate::events::{self, Level};
use crate::net::{self, ChannelSlot, Trust};
use crate::state::{Coop, Link, SessionInfo, Shared};

const CONFIG_CHECK_EVERY: Duration = Duration::from_secs(2);
const CONNECT_WARN: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq)]
enum Plan {
    /// Diagnostics code and the reason in words.
    Offline(&'static str, String),
    Manual { server: String, room: String },
    NightCoop { host: bool },
}

fn decide(s: &Settings, nc: &Coop) -> Plan {
    let manual = |code: &'static str, why: String| {
        if s.server.is_empty() {
            Plan::Offline(code, why)
        } else {
            Plan::Manual { server: s.server.clone(), room: s.room.clone() }
        }
    };
    if s.session_mode == "manual" {
        return manual("manual_no_server", tr!("modalità manuale senza server", "manual mode without a server"));
    }
    match nc {
        Coop::Unknown | Coop::Absent => manual("nc_missing", tr!("NightCoop non trovato", "NightCoop not found")),
        // NightCoop is there but we can't use it: never route around it with our own network.
        Coop::NoSdk(why) => Plan::Offline("nc_sdk_off", why.clone()),
        Coop::Refused => Plan::Offline("nc_sdk_refused", tr!("NightCoop ha rifiutato la registrazione di VoiceChat", "NightCoop refused to register VoiceChat")),
        Coop::Ready(st) if !st.in_session => {
            Plan::Offline("nc_no_session", tr!("non sei in una sessione NightCoop", "you are not in a NightCoop session"))
        }
        Coop::Ready(st) => Plan::NightCoop { host: st.is_host },
    }
}

/// Something running for the current plan; dropped to stop it.
struct Running {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    started: Instant,
    warned: bool,
    phase: &'static str,
    warn_code: &'static str,
    warn_text: String,
}

impl Running {
    fn new(phase: &'static str) -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            started: Instant::now(),
            warned: false,
            phase,
            warn_code: "",
            warn_text: String::new(),
        }
    }

    fn connection(&mut self, shared: &Arc<Shared>, slot: &Arc<ChannelSlot>, params: net::Params) {
        let (shared, slot) = (shared.clone(), slot.clone());
        if let Ok(t) = thread::Builder::new().name("voicechat-net".into()).spawn(move || net::run(shared, slot, params)) {
            self.threads.push(t);
        }
    }

    /// Warn once if we haven't joined after a while.
    fn watch_join(&mut self, code: &'static str, text: String) {
        self.warn_code = code;
        self.warn_text = text;
    }

    fn tick(&mut self, shared: &Shared) {
        let joined = shared.lock(&shared.link).joined().is_some();
        if joined {
            self.warned = false;
            self.started = Instant::now();
        } else if !self.warned && !self.warn_code.is_empty() && self.started.elapsed() > CONNECT_WARN {
            self.warned = true;
            events::report(Level::Warn, self.phase, self.warn_code, self.warn_text.clone());
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

pub fn run(shared: Arc<Shared>, slot: Arc<ChannelSlot>) {
    let mut watcher = ConfigWatcher::new();
    let mut plan: Option<Plan> = None;
    let mut running: Option<Running> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        watcher.tick(&shared);
        let nc = shared.lock(&shared.coop).clone();
        // Until the game thread has looked for NightCoop, don't guess (we would
        // briefly fall back to manual mode).
        if nc == Coop::Unknown {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        let next = decide(&shared.settings(), &nc);
        if plan.as_ref() != Some(&next) {
            // Stop the old plan first.
            drop(running.take());
            {
                // The game thread writes the roster and link under this lock while `coop_voice` is on.
                let _link = shared.lock(&shared.link);
                shared.coop_voice.store(false, Ordering::Relaxed);
            }
            slot.clear();
            shared.lock(&shared.speakers).clear();
            shared.lock(&shared.roster).clear();
            *shared.lock(&shared.via) = String::new();
            running = start(&shared, &slot, &next);
            plan = Some(next);
        }
        if let Some(r) = running.as_mut() {
            r.tick(&shared);
        }
        thread::sleep(Duration::from_millis(200));
    }
    drop(running);
    shared.save_if_due(true);
}

fn set_info(shared: &Shared, f: impl FnOnce(&mut SessionInfo)) {
    f(&mut shared.lock(&shared.session));
}

fn start(shared: &Arc<Shared>, slot: &Arc<ChannelSlot>, plan: &Plan) -> Option<Running> {
    match plan {
        Plan::Offline(code, why) => {
            *shared.lock(&shared.link) = Link::Off(why.clone());
            set_info(shared, |i| *i = SessionInfo { mode: "offline".into(), detail: why.clone() });
            let level = if code.starts_with("nc_sdk") { Level::Error } else { Level::Info };
            events::report(level, "Session", code, why.clone());
            None
        }
        Plan::Manual { server, room } => Some(start_manual(shared, slot, server, room)),
        Plan::NightCoop { host } => {
            *shared.lock(&shared.link) = Link::Connecting;
            let mode = if *host { "host" } else { "guest" }.to_owned();
            set_info(shared, |i| *i = SessionInfo { mode, detail: tr!("tramite NightCoop", "through NightCoop") });
            *shared.lock(&shared.via) = "NightCoop".into();
            shared.coop_voice.store(true, Ordering::Relaxed);
            for code in ["nc_missing", "nc_no_session", "nc_sdk_off", "nc_sdk_refused"] {
                events::resolve(code);
            }
            let role = if *host { tr!("sei l'host", "you are the host") } else { tr!("sei un ospite", "you are a guest") };
            events::report(Level::Info, "Session", "mode", tr!("sessione NightCoop ({role}): la voce passa da NightCoop", "NightCoop session ({role}): voice goes through NightCoop"));
            None
        }
    }
}

/// `host:port#FINGERPRINT` → (resolved addresses, fingerprint).
fn parse_server(s: &str) -> Result<(Vec<SocketAddr>, String), String> {
    let (addr, fp) = s.split_once('#').map_or((s, ""), |(a, f)| (a, f));
    let addr = addr.trim();
    let with_port = if addr.contains(':') { addr.to_owned() } else { format!("{addr}:{}", vc_protocol::DEFAULT_PORT) };
    let addrs: Vec<SocketAddr> = with_port
        .to_socket_addrs()
        .map_err(|e| format!("'{addr}': {e}"))?
        .filter(SocketAddr::is_ipv4)
        .collect();
    if addrs.is_empty() {
        return Err(tr!("'{addr}': nessun indirizzo IPv4", "'{addr}': no IPv4 address"));
    }
    Ok((addrs, fp.trim().to_ascii_uppercase()))
}

fn client_socket() -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind(("0.0.0.0", 0))?;
    s.set_read_timeout(Some(Duration::from_millis(50)))?;
    Ok(s)
}

fn start_manual(shared: &Arc<Shared>, slot: &Arc<ChannelSlot>, server: &str, room: &str) -> Running {
    let mut r = Running::new("Server");
    *shared.lock(&shared.link) = Link::Connecting;
    set_info(shared, |i| *i = SessionInfo { mode: "manual".into(), detail: tr!("{server}, stanza {room}", "{server}, room {room}") });
    events::report(Level::Info, "Session", "mode", tr!("modalità manuale: server {server}", "manual mode: server {server}"));
    let (targets, fp) = match parse_server(server) {
        Ok(v) => v,
        Err(e) => {
            events::report(Level::Error, "Server", "server_unreachable", tr!("server {e}", "server {e}"));
            *shared.lock(&shared.link) = Link::Failed(e);
            return r;
        }
    };
    let Ok(sock) = client_socket() else { return r };
    let secret = Secret::for_room(room);
    let trust = if fp.is_empty() { Trust::PinInSettings } else { Trust::Fingerprint };
    r.connection(shared, slot, net::Params {
        sock,
        targets: Arc::new(Mutex::new(targets)),
        psk: [0; 32],
        trust,
        fingerprint: fp,
        room: secret.room_id(),
        voice_key: secret.voice_key(),
        via: if crate::i18n::italian() { "manuale" } else { "manual" },
        phase: "Server",
        stop: r.stop.clone(),
    });
    r.watch_join("server_unreachable", tr!("nessuna risposta da {server}", "no answer from {server}"));
    r
}

/// Saves settings changed in the UI and follows hand edits of config.ini.
struct ConfigWatcher {
    mtime: Option<std::time::SystemTime>,
    last_check: Instant,
}

impl ConfigWatcher {
    fn new() -> Self {
        Self { mtime: config::modified(), last_check: Instant::now() }
    }

    fn tick(&mut self, shared: &Shared) {
        if shared.save_if_due(false) {
            // Our own write: don't reload it.
            self.mtime = config::modified();
        }
        if self.last_check.elapsed() < CONFIG_CHECK_EVERY {
            return;
        }
        self.last_check = Instant::now();
        let mtime = config::modified();
        if mtime != self.mtime {
            self.mtime = mtime;
            shared.replace(config::load());
            crate::log_info(format_args!("config: reloaded"));
        }
    }
}
