//! Settings, stored in `config.ini` next to the DLL.
//!
//! The file is rewritten (with comments) when the in-game UI changes
//! something, and re-read when edited by hand while the game runs.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::keys;
use crate::log_info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    PushToTalk,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noise {
    Off,
    Low,
    Medium,
    High,
    Max,
    /// RNNoise (neural network).
    Ai,
}

impl Noise {
    const ALL: [(Noise, &'static str); 6] = [
        (Noise::Off, "off"),
        (Noise::Low, "low"),
        (Noise::Medium, "medium"),
        (Noise::High, "high"),
        (Noise::Max, "max"),
        (Noise::Ai, "ai"),
    ];

    fn name(self) -> &'static str {
        Self::ALL.iter().find(|(n, _)| *n == self).unwrap().1
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, name)| *name == s).map(|(n, _)| *n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerPref {
    pub volume: f32,
    pub muted: bool,
}

impl Default for PlayerPref {
    fn default() -> Self {
        Self { volume: 1.0, muted: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Interface language: `en` (default) or `it`.
    pub language: String,
    /// `auto`: follow the NightCoop session (host or guest). `manual`: the
    /// server and room below.
    pub session_mode: String,
    /// The guided setup has been completed (or skipped).
    pub setup_done: bool,
    // Manual mode.
    pub server: String,
    pub server_key: String,
    pub room: String,
    pub name: String,
    // Applied live.
    pub party: String,
    pub input_device: String,
    pub output_device: String,
    pub mode: Mode,
    pub ptt_vk: i32,
    /// Open-mic threshold in dBFS.
    pub vad_threshold_db: f32,
    pub input_gain_db: f32,
    pub mic_volume: f32,
    pub voice_volume: f32,
    pub echo_cancel: bool,
    pub noise: Noise,
    pub mic_muted: bool,
    pub deafened: bool,
    /// Hear your own processed microphone (mic test).
    pub monitor: bool,
    pub occlusion: bool,
    /// Steam Audio for walls, rebounds and 3D sound (off: the classic model).
    pub steam_audio: bool,
    /// How loud Steam Audio's rebounds are (1 = as tuned).
    pub rebounds: f32,
    pub reverb: bool,
    /// Hear the reverb of your own voice in reverberant places.
    pub self_reverb: bool,
    pub self_reverb_level: f32,
    pub hud: bool,
    pub hud_x: f32,
    pub hud_y: f32,
    pub hud_scale: f32,
    pub hud_opacity: f32,
    /// By player name.
    pub players: BTreeMap<String, PlayerPref>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "en".into(),
            session_mode: "auto".into(),
            setup_done: false,
            server: String::new(),
            server_key: String::new(),
            room: "nightcity".into(),
            name: String::new(),
            party: String::new(),
            input_device: String::new(),
            output_device: String::new(),
            mode: Mode::PushToTalk,
            ptt_vk: 0x05,
            vad_threshold_db: -45.0,
            input_gain_db: 0.0,
            mic_volume: 1.0,
            voice_volume: 1.0,
            echo_cancel: true,
            noise: Noise::Medium,
            mic_muted: false,
            deafened: false,
            monitor: false,
            occlusion: true,
            steam_audio: true,
            rebounds: 1.0,
            reverb: true,
            self_reverb: false,
            self_reverb_level: 0.35,
            hud: true,
            hud_x: 1577.0,
            hud_y: 996.0,
            hud_scale: 0.88,
            hud_opacity: 1.0,
            players: BTreeMap::new(),
        }
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" | "si" | "sì" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn parse_f32(s: &str, lo: f32, hi: f32) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite()).map(|v| v.clamp(lo, hi))
}

impl Settings {
    /// Reads one setting as text, for the UI.
    pub fn get(&self, key: &str) -> Option<String> {
        Some(match key {
            "language" => self.language.clone(),
            "session_mode" => self.session_mode.clone(),
            "setup_done" => on_off(self.setup_done).into(),
            "server" => self.server.clone(),
            "server_key" => self.server_key.clone(),
            "room" => self.room.clone(),
            "name" => self.name.clone(),
            "party" => self.party.clone(),
            "input_device" => self.input_device.clone(),
            "output_device" => self.output_device.clone(),
            "mode" => if self.mode == Mode::Open { "open" } else { "ptt" }.into(),
            "ptt" => keys::name(self.ptt_vk),
            "ptt_vk" => self.ptt_vk.to_string(),
            "vad_threshold_db" => self.vad_threshold_db.to_string(),
            "input_gain_db" => self.input_gain_db.to_string(),
            "mic_volume" => self.mic_volume.to_string(),
            "voice_volume" => self.voice_volume.to_string(),
            "echo_cancel" => on_off(self.echo_cancel).into(),
            "noise" => self.noise.name().into(),
            "mic_muted" => on_off(self.mic_muted).into(),
            "deafened" => on_off(self.deafened).into(),
            "monitor" => on_off(self.monitor).into(),
            "occlusion" => on_off(self.occlusion).into(),
            "steam_audio" => on_off(self.steam_audio).into(),
            "rebounds" => self.rebounds.to_string(),
            "reverb" => on_off(self.reverb).into(),
            "self_reverb" => on_off(self.self_reverb).into(),
            "self_reverb_level" => self.self_reverb_level.to_string(),
            "hud" => on_off(self.hud).into(),
            "hud_x" => self.hud_x.to_string(),
            "hud_y" => self.hud_y.to_string(),
            "hud_scale" => self.hud_scale.to_string(),
            "hud_opacity" => self.hud_opacity.to_string(),
            _ => return None,
        })
    }

    /// Changes one setting from text. Unknown keys and bad values are errors.
    pub fn set(&mut self, key: &str, val: &str) -> Result<(), String> {
        let val = val.trim();
        let bad = || format!("bad value '{val}' for {key}");
        let b = || parse_bool(val).ok_or_else(bad);
        let f = |lo, hi| parse_f32(val, lo, hi).ok_or_else(bad);
        match key {
            "language" => {
                if val != "en" && val != "it" {
                    return Err(bad());
                }
                self.language = val.into();
            }
            "session_mode" => {
                if val != "auto" && val != "manual" {
                    return Err(bad());
                }
                self.session_mode = val.into();
            }
            // The old own-network settings (DHT, STUN, UPnP, host port, host relay): ignored.
            "host_port" | "upnp" | "host_relay" | "dht" | "stun_servers" => {}
            "setup_done" => self.setup_done = b()?,
            "server" => self.server = val.into(),
            "server_key" => self.server_key = val.to_ascii_uppercase(),
            "room" => self.room = vc_protocol::clean_name(val),
            "name" => self.name = vc_protocol::clean_name(val),
            "party" => self.party = vc_protocol::clean_name(val),
            "input_device" => self.input_device = val.into(),
            "output_device" => self.output_device = val.into(),
            "mode" => {
                self.mode = match val {
                    "open" | "vox" | "voice" => Mode::Open,
                    "ptt" | "push" => Mode::PushToTalk,
                    _ => return Err(bad()),
                }
            }
            "ptt" => self.ptt_vk = keys::parse(val).ok_or_else(bad)?,
            "ptt_vk" => {
                self.ptt_vk = val.parse().ok().filter(|v| (1..255).contains(v)).ok_or_else(bad)?
            }
            "vad_threshold_db" => self.vad_threshold_db = f(-80.0, 0.0)?,
            "input_gain_db" => self.input_gain_db = f(-20.0, 30.0)?,
            "mic_volume" => self.mic_volume = f(0.0, 2.0)?,
            // `volume` and `mic_gain` are the 0.1 names.
            "voice_volume" | "volume" => self.voice_volume = f(0.0, 2.0)?,
            "mic_gain" => self.mic_volume = f(0.0, 2.0)?,
            "echo_cancel" => self.echo_cancel = b()?,
            "noise" => self.noise = Noise::parse(val).ok_or_else(bad)?,
            "mic_muted" => self.mic_muted = b()?,
            "deafened" => self.deafened = b()?,
            "monitor" => self.monitor = b()?,
            "occlusion" => self.occlusion = b()?,
            "steam_audio" => self.steam_audio = b()?,
            "rebounds" => self.rebounds = f(0.0, 2.0)?,
            "reverb" => self.reverb = b()?,
            "self_reverb" => self.self_reverb = b()?,
            "self_reverb_level" => self.self_reverb_level = f(0.0, 1.0)?,
            "hud" => self.hud = b()?,
            // Dropped in 0.2: the HUD no longer lists who is talking.
            "hud_names" => {}
            "hud_x" => self.hud_x = f(0.0, 3840.0)?,
            "hud_y" => self.hud_y = f(0.0, 2160.0)?,
            "hud_scale" => self.hud_scale = f(0.5, 2.5)?,
            "hud_opacity" => self.hud_opacity = f(0.1, 1.0)?,
            // Dropped in 0.2: the indicator always lives in the crosshair
            // container, the only one where it shows.
            "hud_parent" => {}
            _ => {
                if let Some(name) = key.strip_prefix("volume.") {
                    self.players.entry(name.into()).or_default().volume = f(0.0, 3.0)?;
                } else if let Some(name) = key.strip_prefix("mute.") {
                    self.players.entry(name.into()).or_default().muted = b()?;
                } else {
                    return Err(format!("unknown setting {key}"));
                }
            }
        }
        Ok(())
    }

    pub fn player(&self, name: &str) -> PlayerPref {
        self.players.get(name).copied().unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with(['#', ';']) {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else { continue };
            if let Err(e) = s.set(key.trim(), val) {
                log_info(format_args!("config: {e}"));
            }
        }
        s
    }

    pub fn to_ini(&self) -> String {
        let mut o = String::new();
        let g = |k: &str| self.get(k).unwrap_or_default();
        // (italiano, english, key)
        let entries: &[(&str, &str, &str)] = &[
            ("VoiceChat. Lingua: en (english) o it (italiano). Tutto si puo' cambiare dal menu di CET (overlay).", "VoiceChat. Language: en (English) or it (Italian). Everything can be changed in the CET overlay.", "language"),
            ("auto = segue la sessione NightCoop (host o ospite); manual = server e stanza qui sotto.", "auto = follow the NightCoop session (host or guest); manual = server and room below.", "session_mode"),
            ("", "", "setup_done"),
            ("Modalita' manuale: server (indirizzo:porta), impronta della sua chiave (si riempie da sola), stanza.", "Manual mode: server (address:port), its key fingerprint (filled in by itself), room.", "server"),
            ("", "", "server_key"),
            ("", "", "room"),
            ("Vuoto = il tuo nome di NightCoop.", "Empty = your NightCoop name.", "name"),
            ("Party: i membri dello stesso party si sentono a qualsiasi distanza.", "Party: members of the same party hear each other at any distance.", "party"),
            ("Dispositivi audio: vuoto = predefinito di Windows.", "Audio devices: empty = Windows default.", "input_device"),
            ("", "", "output_device"),
            ("ptt = push-to-talk, open = microfono aperto (si attiva sopra la soglia).", "ptt = push-to-talk, open = open mic (opens above the threshold).", "mode"),
            ("Tasto push-to-talk: mouse4, mouse5, una lettera, F1-F24, o un codice VK (0x14).", "Push-to-talk key: mouse4, mouse5, a letter, F1-F24, or a VK code (0x14).", "ptt"),
            ("Soglia del microfono aperto, in dB (-80 = sensibilissimo, 0 = mai).", "Open mic threshold, in dB (-80 = very sensitive, 0 = never).", "vad_threshold_db"),
            ("Guadagno del microfono prima dell'elaborazione, in dB.", "Microphone gain before processing, in dB.", "input_gain_db"),
            ("Volume del tuo microfono e della chat vocale (1.0 = normale).", "Volume of your microphone and of the voice chat (1.0 = normal).", "mic_volume"),
            ("", "", "voice_volume"),
            ("Cancellazione dell'eco (per chi usa le casse).", "Echo cancellation (for speakers).", "echo_cancel"),
            ("Soppressione del rumore: off, low, medium, high, max, ai.", "Noise suppression: off, low, medium, high, max, ai.", "noise"),
            ("Microfono disattivato / chat vocale silenziata (spegne anche il microfono).", "Microphone muted / voice chat muted (mutes the microphone too).", "mic_muted"),
            ("", "", "deafened"),
            ("Senti il tuo microfono (prova).", "Hear your own microphone (test).", "monitor"),
            ("Muri che attutiscono la voce, riverbero degli ambienti.", "Walls muffling voices, room reverb.", "occlusion"),
            ("Acustica con Steam Audio: rimbalzi, materiali, suono 3D (off: modello classico).", "Acoustics with Steam Audio: rebounds, materials, 3D sound (off: classic model).", "steam_audio"),
            ("", "", "rebounds"),
            ("", "", "reverb"),
            ("Senti il riverbero della tua voce negli ambienti chiusi, e quanto.", "Hear the reverb of your own voice in enclosed places, and how much.", "self_reverb"),
            ("", "", "self_reverb_level"),
            ("Icona nell'HUD: posizione (pixel su 1920x1080), scala, opacita'.", "HUD icon: position (pixels on 1920x1080), scale, opacity.", "hud"),
            ("", "", "hud_x"),
            ("", "", "hud_y"),
            ("", "", "hud_scale"),
            ("", "", "hud_opacity"),
        ];
        let italian = self.language == "it";
        for (it, en, key) in entries {
            let comment = if italian { it } else { en };
            if !comment.is_empty() {
                let _ = writeln!(o, "\n# {comment}");
            }
            let _ = writeln!(o, "{key} = {}", g(*key));
        }
        if !self.players.is_empty() {
            let _ = writeln!(o, "\n# {}", if italian { "Volume e silenziamento per giocatore." } else { "Volume and mute per player." });
        }
        for (name, p) in &self.players {
            let _ = writeln!(o, "volume.{name} = {}", p.volume);
            let _ = writeln!(o, "mute.{name} = {}", on_off(p.muted));
        }
        o
    }
}

/// `<game>/bin/x64/Cyberpunk2077.exe` → `<game>`.
fn game_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.parent()?.to_path_buf())
}

pub fn path() -> PathBuf {
    game_root().unwrap_or_default().join("red4ext/plugins/VoiceChat/config.ini")
}

/// Last modification of config.ini, to notice edits while the game runs.
pub fn modified() -> Option<SystemTime> {
    fs::metadata(path()).and_then(|m| m.modified()).ok()
}

pub fn load() -> Settings {
    match fs::read_to_string(path()) {
        Ok(text) => Settings::parse(&text),
        Err(_) => {
            let s = Settings::default();
            save(&s);
            s
        }
    }
}

pub fn save(s: &Settings) {
    let path = path();
    // Write then rename, so a crash never leaves half a file.
    let tmp = path.with_extension("ini.tmp");
    if let Err(e) = fs::write(&tmp, s.to_ini()).and_then(|_| fs::rename(&tmp, &path)) {
        log_info(format_args!("config: cannot write {}: {e}", path.display()));
    }
}

/// The name players see: the configured one, else NightCoop's, else "V".
pub fn display_name(s: &Settings) -> String {
    if !s.name.is_empty() {
        return s.name.clone();
    }
    nightcoop_name(&game_root().unwrap_or_default()).unwrap_or_else(|| "V".into())
}

fn nightcoop_name(root: &Path) -> Option<String> {
    let name = fs::read_to_string(root.join("red4ext/plugins/NightCoop/name.txt")).ok()?;
    let name = vc_protocol::clean_name(&name);
    (!name.is_empty()).then_some(name)
}
