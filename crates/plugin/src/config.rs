//! `config.ini` next to the DLL. Written with defaults on first start.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::log_info;

const DEFAULT_INI: &str = "\
# VoiceChat: edit with the game closed.

# Voice server (address:port), e.g. 203.0.113.7:47900.
server =
# Players with the same room code hear each other.
room = nightcity
# Empty = your NightCoop name.
name =
# Party joined at start; party members hear each other at any distance.
party =
# Push-to-talk: mouse4, mouse5, a letter (e.g. V), F1-F12, or a Windows VK code (e.g. 0x14).
ptt = mouse4
# Volume of the other players and of your microphone (1.0 = normal).
volume = 1.0
mic_gain = 1.0
";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub server: String,
    pub room: String,
    pub name: String,
    pub party: String,
    pub ptt_vk: i32,
    pub volume: f32,
    pub mic_gain: f32,
}

/// `<game>/red4ext/plugins/VoiceChat`, derived from `<game>/bin/x64/Cyberpunk2077.exe`.
fn game_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.parent()?.to_path_buf())
}

fn path() -> PathBuf {
    game_root().unwrap_or_default().join("red4ext/plugins/VoiceChat/config.ini")
}

/// Last modification of config.ini, to notice edits while the game runs.
pub fn modified() -> Option<SystemTime> {
    fs::metadata(path()).and_then(|m| m.modified()).ok()
}

pub fn load() -> Config {
    let root = game_root().unwrap_or_default();
    let path = path();
    let text = fs::read_to_string(&path).unwrap_or_else(|_| {
        if let Err(e) = fs::write(&path, DEFAULT_INI) {
            log_info(format_args!("config: cannot write {}: {e}", path.display()));
        }
        DEFAULT_INI.to_owned()
    });
    let mut cfg = parse(&text);
    if cfg.name.is_empty() {
        cfg.name = nightcoop_name(&root).unwrap_or_else(|| "V".into());
    }
    cfg
}

fn nightcoop_name(root: &Path) -> Option<String> {
    let name = fs::read_to_string(root.join("red4ext/plugins/NightCoop/name.txt")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn parse(text: &str) -> Config {
    let mut cfg = Config {
        server: String::new(),
        room: String::new(),
        name: String::new(),
        party: String::new(),
        ptt_vk: 0x05,
        volume: 1.0,
        mic_gain: 1.0,
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else { continue };
        let (key, val) = (key.trim(), val.trim());
        match key {
            "server" => cfg.server = val.into(),
            "room" => cfg.room = val.into(),
            "name" => cfg.name = val.into(),
            "party" => cfg.party = val.into(),
            "ptt" => match parse_key(val) {
                Some(vk) => cfg.ptt_vk = vk,
                None => log_info(format_args!("config: unknown ptt key '{val}', using mouse4")),
            },
            "volume" => cfg.volume = val.parse().unwrap_or(1.0),
            "mic_gain" => cfg.mic_gain = val.parse().unwrap_or(1.0),
            _ => log_info(format_args!("config: unknown key '{key}'")),
        }
    }
    cfg
}

/// Windows virtual-key code for a key name.
fn parse_key(s: &str) -> Option<i32> {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "mouse4" => return Some(0x05),
        "mouse5" => return Some(0x06),
        _ => {}
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return i32::from_str_radix(hex, 16).ok();
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<i32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    match s.as_bytes() {
        [c] if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as i32),
        _ => s.parse().ok(),
    }
}
