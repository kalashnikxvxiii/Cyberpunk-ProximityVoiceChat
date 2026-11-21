//! Global functions for the CET mod (`Game.VoiceChat_*` in Lua).
//!
//! Lists come back as arrays of tab-separated lines; the Lua side splits them.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use red4ext_rs::{Exportable, GlobalExport, exports, global};
use vc_dsp::spatial::Path;

use crate::state::{Coop, Link, Shared, Surroundings};
use crate::{keys, shared};

pub fn exports() -> impl Exportable {
    exports![
        GlobalExport(global!(c"VoiceChat_Version", version)),
        GlobalExport(global!(c"VoiceChat_Status", status)),
        GlobalExport(global!(c"VoiceChat_Get", get)),
        GlobalExport(global!(c"VoiceChat_Set", set)),
        GlobalExport(global!(c"VoiceChat_Devices", devices)),
        GlobalExport(global!(c"VoiceChat_RescanDevices", rescan_devices)),
        GlobalExport(global!(c"VoiceChat_Meters", meters)),
        GlobalExport(global!(c"VoiceChat_Players", players)),
        GlobalExport(global!(c"VoiceChat_Speakers", speakers)),
        GlobalExport(global!(c"VoiceChat_SetPath", set_path)),
        GlobalExport(global!(c"VoiceChat_SetPathAlt", set_path_alt)),
        GlobalExport(global!(c"VoiceChat_SetSurroundings", set_surroundings)),
        GlobalExport(global!(c"VoiceChat_SetPersonal", set_personal)),
        GlobalExport(global!(c"VoiceChat_SetEar", set_ear)),
        GlobalExport(global!(c"VoiceChat_CaptureKey", capture_key)),
        GlobalExport(global!(c"VoiceChat_CapturedKey", captured_key)),
        GlobalExport(global!(c"VoiceChat_KeyName", key_name)),
        GlobalExport(global!(c"VoiceChat_NewRoomCode", new_room_code)),
        GlobalExport(global!(c"VoiceChat_JoinParty", join_party)),
        GlobalExport(global!(c"VoiceChat_LeaveParty", leave_party)),
        GlobalExport(global!(c"VoiceChat_Log", log)),
        GlobalExport(global!(c"VoiceChat_Events", events_list)),
        GlobalExport(global!(c"VoiceChat_ClearEvents", events_clear)),
        GlobalExport(global!(c"VoiceChat_Event", event)),
        GlobalExport(global!(c"VoiceChat_ResolveEvent", resolve_event)),
    ]
}

fn with<T: Default>(f: impl FnOnce(&Arc<Shared>) -> T) -> T {
    shared().map(|s| f(&s)).unwrap_or_default()
}

fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

/// `key=value` lines describing the connection and the audio engine.
fn status() -> Vec<String> {
    with(|s| {
        let set = s.settings();
        let (link, detail) = match &*s.lock(&s.link) {
            Link::Off(m) => ("off", m.clone()),
            Link::Connecting => ("connecting", String::new()),
            Link::Joined { id, range } => ("joined", format!("#{id}, {range} m")),
            Link::Failed(m) => ("failed", m.clone()),
        };
        let info = s.lock(&s.session).clone();
        let coop = s.lock(&s.coop);
        let (members, voice) = match &*coop {
            Coop::Ready(st) if st.in_session => (members(st), voice(&st.metrics)),
            _ => (String::new(), String::new()),
        };
        let nc = match &*coop {
            Coop::Unknown => "?".into(),
            Coop::Absent => crate::tr!("assente", "not found"),
            Coop::NoSdk(why) => why.clone(),
            Coop::Refused => crate::tr!("registrazione rifiutata", "registration refused"),
            Coop::Ready(st) if !st.in_session => crate::tr!("nessuna sessione", "no session"),
            Coop::Ready(st) => {
                let role = if st.is_host { "host".to_owned() } else { crate::tr!("ospite", "guest") };
                let with = st.members.iter().filter(|m| m.has_mod).count();
                crate::tr!("{}, {} altri giocatori ({} con VoiceChat)", "{}, {} other players ({} with VoiceChat)", role, st.members.len(), with)
            }
        };
        drop(coop);
        vec![
            format!("mode={}", info.mode),
            format!("mode_detail={}", info.detail),
            format!("via={}", s.lock(&s.via)),
            format!("nightcoop={nc}"),
            format!("nc_build={}", s.lock(&s.nc_build)),
            format!("ptt_clash={}", s.lock(&s.ptt_clash)),
            format!("nc_members={members}"),
            format!("nc_voice={voice}"),
            format!("link={link}"),
            format!("link_detail={detail}"),
            format!("fingerprint={}", s.lock(&s.server_fingerprint)),
            format!("audio={}", s.lock(&s.audio_status)),
            format!("name={}", s.name),
            format!("room={}", set.room),
            format!("server={}", set.server),
            format!("in_game={}", s.lock(&s.pose).is_some() as u8),
        ]
    })
}

/// The other players of the session, and whether they have the mod.
fn members(st: &vc_coop::chat::Status) -> String {
    let mut v: Vec<String> = st
        .members
        .iter()
        .map(|m| if m.has_mod { m.name.clone() } else { crate::tr!("{} (senza VoiceChat)", "{} (no VoiceChat)", m.name) })
        .collect();
    v.sort();
    v.join(", ")
}

/// The voice through NightCoop, as measured by the chat.
fn voice(m: &vc_coop::chat::Metrics) -> String {
    let lost = 100.0 * m.lost as f32 / (m.frames + m.lost).max(1) as f32;
    crate::tr!(
        "latenza {:.0} ms, jitter {:.0} ms, {} frame ricevuti, persi in rete {:.1}%, {} non inviati da chi parla, {} scartati, {} rifiutati, {} volte al limite di NightCoop, {} blocchi del gioco",
        "latency {:.0} ms, jitter {:.0} ms, {} frames received, {:.1}% lost in the network, {} not sent by the speaker, {} dropped, {} refused, {} times at NightCoop's limit, {} game stalls",
        m.latency_ms,
        m.jitter_ms,
        m.frames,
        lost,
        m.skipped,
        m.dropped,
        m.refused,
        m.limited,
        m.stalls
    )
}

fn get(key: String) -> String {
    with(|s| s.settings().get(&key).unwrap_or_default())
}

/// Empty on success, else the error.
fn set(key: String, value: String) -> String {
    with(|s| s.update(|set| set.set(&key, &value)).err().unwrap_or_default())
}

fn devices(input: bool) -> Vec<String> {
    with(|s| {
        let d = s.lock(&s.devices);
        if input { d.0.clone() } else { d.1.clone() }
    })
}

fn rescan_devices() {
    with(|s| s.rescan_devices.store(true, Ordering::Relaxed));
}

/// `[input dB, processed dB, gate open, transmitting, threshold dB]`.
fn meters() -> Vec<f32> {
    with(|s| {
        let m = *s.lock(&s.meters);
        vec![
            m.input_db,
            m.processed_db,
            m.gate_open as u8 as f32,
            m.transmitting as u8 as f32,
            s.settings().vad_threshold_db,
        ]
    })
}

/// One line per player in the room:
/// `id \t name \t party \t talking \t volume \t muted \t level dB \t is me`.
fn players() -> Vec<String> {
    with(|s| {
        let set = s.settings();
        let me = s.lock(&s.link).joined().map(|(id, _)| id);
        let speakers = s.lock(&s.speakers);
        let now = Instant::now();
        s.lock(&s.roster)
            .iter()
            .map(|p| {
                let pref = set.player(&p.name);
                let info = speakers.get(&p.id);
                let talking = info.and_then(|i| i.audible_until).is_some_and(|t| now < t);
                let is_me = me == Some(p.id);
                let talking = if is_me { s.lock(&s.meters).transmitting } else { talking };
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{:.0}\t{}",
                    p.id,
                    p.name,
                    p.party,
                    talking as u8,
                    pref.volume,
                    pref.muted as u8,
                    info.map_or(-100.0, |i| i.level_db),
                    is_me as u8
                )
            })
            .collect()
    })
}

/// Speakers heard in the last 10 s (their paths stay ready through pauses), for the acoustics:
/// `id \t x \t y \t z \t party \t ears above the feet (m)`.
fn speakers() -> Vec<String> {
    with(|s| {
        s.lock(&s.speakers)
            .iter()
            .filter(|(_, i)| i.last_packet.elapsed() < Duration::from_secs(10))
            .filter_map(|(id, i)| {
                // Only people in the world have a place for sound to travel from.
                let p = i.pos?;
                let ear = i.env.ear_m().unwrap_or(vc_dsp::spatial::EAR_HEIGHT);
                Some(format!("{id}\t{}\t{}\t{}\t{}\t{ear}", p[0], p[1], p[2], i.party as u8))
            })
            .collect()
    })
}

fn set_path(id: i32, x: f32, y: f32, z: f32, gain: f32, cutoff: f32) {
    let Ok(id) = u16::try_from(id) else { return };
    if ![x, y, z, gain, cutoff].iter().all(|v| v.is_finite()) {
        return;
    }
    with(|s| s.set_path(id, Path { from: [x, y, z], gain: gain.clamp(0.0, 1.0), cutoff }));
}

/// A second way the voice of `id` reaches us (gain 0: none).
fn set_path_alt(id: i32, x: f32, y: f32, z: f32, gain: f32, cutoff: f32) {
    let Ok(id) = u16::try_from(id) else { return };
    if ![x, y, z, gain, cutoff].iter().all(|v| v.is_finite()) {
        return;
    }
    with(|s| s.set_path_alt(id, Path { from: [x, y, z], gain: gain.clamp(0.0, 1.0), cutoff }));
}

fn set_surroundings(wet: f32, size: f32, damp: f32) {
    if ![wet, size, damp].iter().all(|v| v.is_finite()) {
        return;
    }
    with(|s| {
        *s.lock(&s.surroundings) = Some(Surroundings {
            wet: wet.clamp(0.0, 1.0),
            size: size.clamp(0.0, 1.0),
            damp: damp.clamp(0.0, 1.0),
            at: Instant::now(),
        })
    });
}

/// Our ears above our feet (m): crouching, seated, standing.
fn set_ear(m: f32) {
    if m.is_finite() {
        with(|s| s.set_ear(m.clamp(0.3, 2.2)));
    }
}

/// What only this player hears now ("radio", "call", "menu"), empty for nothing.
fn set_personal(reason: String) {
    with(|s| s.set_personal(reason.chars().take(32).collect()));
}

fn capture_key() {
    keys::start_capture();
}

/// 0 while waiting, -1 if cancelled, else the key code.
fn captured_key() -> i32 {
    keys::captured()
}

fn key_name(vk: i32) -> String {
    keys::name(vk)
}

/// Diagnostics from the CET mod, into the plugin's log (CET's own log is
/// only flushed at exit).
fn log(msg: String) {
    let clean: String = msg.chars().filter(|c| !c.is_control()).take(500).collect();
    crate::log_info(format_args!("cet: {clean}"));
}

fn events_list() -> Vec<String> {
    crate::events::lines()
}

fn events_clear() {
    crate::events::clear();
}

/// From the CET mod: its own problems (HUD, acoustics), same register.
fn event(level: String, phase: String, code: String, detail: String) {
    let code: String = code.chars().filter(char::is_ascii_alphanumeric).take(32).collect();
    let phase: String = phase.chars().filter(|c| !c.is_control()).take(24).collect();
    crate::events::report(crate::events::Level::parse(&level), &phase, &code, detail);
}

/// The CET mod's problem went away (e.g. the HUD icon attached after all).
fn resolve_event(code: String) {
    crate::events::resolve(&code);
}

/// A random room code for manual mode (80 bits): typed names can be guessed offline by whoever runs the relay.
fn new_room_code() -> String {
    vc_protocol::crypto::fingerprint(&vc_protocol::crypto::random_bytes::<32>())
}

fn join_party(name: String) {
    with(|s| s.update(|set| set.set("party", &name)).ok());
}

fn leave_party() {
    with(|s| s.update(|set| set.set("party", "")).ok());
}
