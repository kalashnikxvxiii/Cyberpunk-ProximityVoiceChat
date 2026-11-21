//! State shared between the game thread (pose, CET calls), the network thread
//! and the audio thread.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vc_dsp::spatial::Path;
use vc_protocol::e2e::VoiceKey;
use vc_protocol::{Env, PlayerId, RosterEntry, Vec3};

use crate::config::{self, Settings};
use crate::game::Pose;

/// Saving waits for the UI to stop changing things (e.g. a dragged slider).
const SAVE_DELAY: Duration = Duration::from_millis(800);
/// Acoustics older than this are ignored (CET mod missing or paused).
const ACOUSTICS_TTL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    Off(String),
    Connecting,
    Joined { id: PlayerId, range: f32 },
    Failed(String),
}

impl Link {
    pub fn joined(&self) -> Option<(PlayerId, f32)> {
        match self {
            Self::Joined { id, range } => Some((*id, *range)),
            _ => None,
        }
    }
}

/// Microphone levels and gate state, for the UI meters and the HUD.
#[derive(Debug, Clone, Copy)]
pub struct Meters {
    pub input_db: f32,
    pub processed_db: f32,
    pub gate_open: bool,
    pub transmitting: bool,
}

impl Default for Meters {
    fn default() -> Self {
        Self { input_db: -100.0, processed_db: -100.0, gate_open: false, transmitting: false }
    }
}

/// Listener surroundings measured by the CET mod.
#[derive(Debug, Clone, Copy)]
pub struct Surroundings {
    /// 0 = open air, 1 = very reverberant.
    pub wet: f32,
    /// 0 = small room, 1 = huge hall or long tunnel.
    pub size: f32,
    /// High-frequency absorption, 0..1.
    pub damp: f32,
    pub at: Instant,
}

impl Surroundings {
    pub fn to_env(self) -> Env {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Env { wet: q(self.wet), size: q(self.size), damp: q(self.damp), ..Env::default() }
    }
}

/// What the session manager is doing, for the UI.
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    /// "host", "guest", "manual", "offline" (the UI words them).
    pub mode: String,
    pub detail: String,
}

/// NightCoop as the game thread found it.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Coop {
    /// Not looked for yet.
    #[default]
    Unknown,
    /// `NightCoop.dll` isn't loaded.
    Absent,
    /// Loaded, but without the SDK we need (too old, or switched off): why.
    NoSdk(String),
    /// NightCoop refused our registration.
    Refused,
    Ready(vc_coop::chat::Status),
}

/// What the audio thread knows about a remote speaker.
pub struct SpeakerInfo {
    /// `None`: not in the world (menus, lobby).
    pub pos: Option<Vec3>,
    pub party: bool,
    pub env: Env,
    pub last_packet: Instant,
    /// Set by the mixer.
    pub level_db: f32,
    pub audible_until: Option<Instant>,
}

/// A voice packet for the audio thread: (from, seq, opus, restart its playback).
pub type Incoming = (PlayerId, u32, Vec<u8>, bool);

pub struct Shared {
    settings: Mutex<Settings>,
    settings_gen: AtomicU64,
    save_at: Mutex<Option<Instant>>,
    pub name: String,
    pub stop: AtomicBool,
    pub pose: Mutex<Option<Pose>>,
    pub link: Mutex<Link>,
    pub server_fingerprint: Mutex<String>,
    pub roster: Mutex<Vec<RosterEntry>>,
    pub speakers: Mutex<HashMap<PlayerId, SpeakerInfo>>,
    pub inbox: Mutex<Vec<Incoming>>,
    /// Our encoded frames for NightCoop's channels, while [`Shared::coop_voice`].
    pub outbox: Mutex<VecDeque<vc_coop::chat::Frame>>,
    /// The voice goes through NightCoop (not our own connection).
    pub coop_voice: AtomicBool,
    /// Per speaker: the loudest way in, a second one from another side, and when they came.
    acoustics: Mutex<HashMap<PlayerId, (Path, Option<Path>, Instant)>>,
    pub surroundings: Mutex<Option<Surroundings>>,
    /// Why the player hears sounds of their own (pocket radio, call, menu), from the CET mod.
    personal: Mutex<Option<(String, Instant)>>,
    /// Our ears above our feet (m), measured by the CET mod.
    ear: Mutex<Option<(f32, Instant)>>,
    pub meters: Mutex<Meters>,
    pub devices: Mutex<(Vec<String>, Vec<String>)>,
    pub audio_status: Mutex<String>,
    pub rescan_devices: AtomicBool,
    /// Seals our voice end to end; set by the active connection.
    pub voice_key: Mutex<Option<VoiceKey>>,
    /// How we are connected, in words (direct, relay, manual).
    pub via: Mutex<String>,
    pub coop: Mutex<Coop>,
    /// NightCoop's build stamp, once registered.
    pub nc_build: Mutex<String>,
    /// What NightCoop also does with our push-to-talk key, empty for nothing (shown under the key).
    pub ptt_clash: Mutex<String>,
    pub session: Mutex<SessionInfo>,
    /// Steam Audio, once started (see `sound3d`).
    pub sound3d: std::sync::OnceLock<crate::sound3d::Sound3d>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic on another thread must not take the game down with it.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn new(settings: Settings) -> Self {
        crate::i18n::set(&settings.language);
        let name = config::display_name(&settings);
        Self {
            settings: Mutex::new(settings),
            settings_gen: AtomicU64::new(1),
            save_at: Mutex::new(None),
            name,
            stop: AtomicBool::new(false),
            pose: Mutex::new(None),
            link: Mutex::new(Link::Connecting),
            server_fingerprint: Mutex::new(String::new()),
            roster: Mutex::new(Vec::new()),
            speakers: Mutex::new(HashMap::new()),
            inbox: Mutex::new(Vec::new()),
            outbox: Mutex::new(VecDeque::new()),
            coop_voice: AtomicBool::new(false),
            acoustics: Mutex::new(HashMap::new()),
            surroundings: Mutex::new(None),
            personal: Mutex::new(None),
            ear: Mutex::new(None),
            meters: Mutex::new(Meters::default()),
            devices: Mutex::new((Vec::new(), Vec::new())),
            audio_status: Mutex::new("starting".into()),
            rescan_devices: AtomicBool::new(true),
            voice_key: Mutex::new(None),
            via: Mutex::new(String::new()),
            coop: Mutex::new(Coop::Unknown),
            nc_build: Mutex::new(String::new()),
            ptt_clash: Mutex::new(String::new()),
            session: Mutex::new(SessionInfo::default()),
            sound3d: std::sync::OnceLock::new(),
        }
    }

    pub fn lock<'a, T>(&self, m: &'a Mutex<T>) -> MutexGuard<'a, T> {
        lock(m)
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Bumped on every change, so threads re-read settings only when needed.
    pub fn settings_gen(&self) -> u64 {
        self.settings_gen.load(Ordering::Acquire)
    }

    /// Changes settings from the UI; saved to config.ini shortly after.
    pub fn update(&self, f: impl FnOnce(&mut Settings) -> Result<(), String>) -> Result<(), String> {
        let mut s = lock(&self.settings);
        let mut next = s.clone();
        f(&mut next)?;
        if next != *s {
            crate::i18n::set(&next.language);
            *s = next;
            self.settings_gen.fetch_add(1, Ordering::AcqRel);
            *lock(&self.save_at) = Some(Instant::now() + SAVE_DELAY);
        }
        Ok(())
    }

    /// Replaces settings after config.ini was edited by hand.
    pub fn replace(&self, new: Settings) {
        let mut s = lock(&self.settings);
        if *s != new {
            crate::i18n::set(&new.language);
            *s = new;
            self.settings_gen.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Writes config.ini if a save is due; true if it wrote.
    pub fn save_if_due(&self, force: bool) -> bool {
        let mut at = lock(&self.save_at);
        match *at {
            Some(t) if force || Instant::now() >= t => {
                *at = None;
                drop(at);
                config::save(&self.settings());
                true
            }
            _ => false,
        }
    }

    pub fn set_path(&self, id: PlayerId, path: Path) {
        let mut a = lock(&self.acoustics);
        let e = a.entry(id).or_insert((path, None, Instant::now()));
        (e.0, e.2) = (path, Instant::now());
    }

    /// The second way in; silent: none.
    pub fn set_path_alt(&self, id: PlayerId, path: Path) {
        if let Some(e) = lock(&self.acoustics).get_mut(&id) {
            e.1 = (path.gain > 0.0).then_some(path);
        }
    }

    /// The acoustic paths to `id` (the loudest, and maybe a second), or the straight line when none is fresh.
    pub fn path(&self, id: PlayerId, pos: Vec3, occlusion: bool) -> (Path, Option<Path>) {
        match lock(&self.acoustics).get(&id) {
            Some((p, alt, at)) if occlusion && at.elapsed() < ACOUSTICS_TTL => (*p, *alt),
            _ => (Path::direct(pos), None),
        }
    }

    /// `reason` empty: none.
    pub fn set_personal(&self, reason: String) {
        *lock(&self.personal) = (!reason.is_empty()).then(|| (reason, Instant::now()));
    }

    /// What only this player hears right now, if anything (stale when the CET mod stops telling).
    pub fn personal(&self) -> Option<String> {
        lock(&self.personal).as_ref().filter(|(_, at)| at.elapsed() < ACOUSTICS_TTL).map(|(r, _)| r.clone())
    }

    pub fn set_ear(&self, m: f32) {
        *lock(&self.ear) = Some((m, Instant::now()));
    }

    /// Our ears above our feet now (standing when the CET mod doesn't tell).
    pub fn ear(&self) -> f32 {
        lock(&self.ear).filter(|(_, at)| at.elapsed() < ACOUSTICS_TTL).map_or(vc_dsp::spatial::EAR_HEIGHT, |(m, _)| m)
    }

    pub fn surroundings(&self) -> Option<Surroundings> {
        lock(&self.surroundings).filter(|s| s.at.elapsed() < ACOUSTICS_TTL)
    }

    pub fn name_of(&self, id: PlayerId) -> Option<String> {
        lock(&self.roster).iter().find(|p| p.id == id).map(|p| p.name.clone())
    }
}
