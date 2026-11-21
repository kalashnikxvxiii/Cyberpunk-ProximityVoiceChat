//! Diagnostics register shown in the CET window: what happened, in which
//! phase, and what the player can do about it. Everything is also logged.
//! The same problem repeating updates its row instead of adding new ones.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::log_info;

const MAX: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "error" => Self::Error,
            "warn" => Self::Warn,
            _ => Self::Info,
        }
    }
}

struct Event {
    time: String,
    level: Level,
    phase: String,
    code: String,
    detail: String,
    hint: String,
    count: u32,
}

static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());

fn clock() -> String {
    let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: GetLocalTime only writes the struct it is given.
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

fn tr(it: &'static str, en: &'static str) -> &'static str {
    if crate::i18n::italian() { it } else { en }
}

/// What to suggest for each kind of problem, in the current language.
pub fn hint(code: &str) -> &'static str {
    match code {
        "nc_missing" => tr("NightCoop non risponde. Se non lo usi, passa alla modalità manuale (Generale) e inserisci un server; altrimenti controlla che NightCoop sia installato e aggiornato.", "NightCoop is not answering. If you don't use it, switch to manual mode (General) and enter a server; otherwise check that NightCoop is installed and up to date."),
        "nc_sdk_off" => tr("NightCoop è installato ma VoiceChat non può usarlo: aggiorna NightCoop, e nelle sue impostazioni lascia attivo l'SDK per le mod (\"sdk\", \"sdk_channels\", \"sdk_state\"). La chat non usa altre strade per non scavalcare le tue scelte.", "NightCoop is installed but VoiceChat can't use it: update NightCoop, and in its settings keep the SDK for mods on (\"sdk\", \"sdk_channels\", \"sdk_state\"). The chat takes no other road, so it doesn't override your choices."),
        "nc_sdk_refused" => tr("NightCoop ha rifiutato VoiceChat: controlla nelle impostazioni di NightCoop che le mod non siano bloccate (\"sdk_block\"), poi riavvia il gioco.", "NightCoop refused VoiceChat: check in NightCoop's settings that mods aren't blocked (\"sdk_block\"), then restart the game."),
        "nc_denied" => tr("L'host della sessione ha spento le mod (\"sdk_session\" nelle impostazioni di NightCoop): finché non lo riaccende la voce non passa.", "The session's host turned mods off (\"sdk_session\" in NightCoop's settings): voice can't get through until they turn it back on."),
        "ptt_nightcoop" => tr("NightCoop non blocca i suoi tasti: ogni volta che parli faresti anche quello che fa lui. Scegli un altro tasto in Generale > Trasmissione.", "NightCoop doesn't block its keys: each time you talk you would also do what it does. Pick another key in General > Transmission."),
        "nc_no_session" => tr("Nessuna azione: la chat si attiva da sola quando ospiti o entri in una sessione co-op di NightCoop.", "Nothing to do: the chat turns on by itself when you host or join a NightCoop co-op session."),
        "version_mismatch" => tr("Il server usa un'altra versione di VoiceChat: aggiornate server e mod alla stessa versione.", "The server uses another version of VoiceChat: update the server and the mod to the same version."),
        "server_unreachable" => tr("Il server non risponde: controlla indirizzo e porta in Generale, che il server sia acceso e che la sua porta UDP sia aperta sul router e nel firewall.", "The server is not answering: check address and port in General, that the server is running and that its UDP port is open on the router and firewall."),
        "server_key_mismatch" => tr("Il server ha una chiave diversa da quella salvata: se l'ha cambiata il gestore del server, svuota 'Chiave del server' in Generale; altrimenti potrebbe essere un server falso, non collegarti.", "The server has a different key from the saved one: if the server's owner changed it, clear 'Saved server key' in General; otherwise it may be a fake server, don't connect."),
        "rejected" => tr("Il server ha rifiutato il collegamento; il motivo è indicato a sinistra.", "The server refused the connection; the reason is on the left."),
        "decrypt_failed" => tr("Arriva voce che non riesco a decifrare: probabilmente qualcuno usa un'altra versione della mod o un'altra stanza.", "Voice is arriving that I can't decrypt: probably someone uses another mod version or another room."),
        "audio_device" => tr("Il dispositivo audio non è disponibile o è stato scollegato. Ricollegalo o scegline un altro in Audio.", "The audio device is unavailable or was unplugged. Plug it back in or pick another one in Audio."),
        "audio_format" => tr("Il dispositivo usa un formato non supportato. Scegli un altro dispositivo in Audio, o imposta il formato a 'float' / 48000 Hz nelle impostazioni audio di Windows.", "The device uses an unsupported format. Pick another device in Audio, or set its format to 48000 Hz in Windows sound settings."),
        "monitor_feedback" => tr("L'ascolto del microfono si è spento perché fischiava: usalo con le cuffie, non con le casse.", "Microphone listening turned off because it was howling: use it with headphones, not speakers."),
        "hud" => tr("L'icona nell'HUD non è disponibile; la chat vocale funziona lo stesso. Prova a ricaricare il salvataggio.", "The HUD icon is unavailable; the voice chat works anyway. Try reloading the save."),
        "acoustics" => tr("Muri e riverbero non funzionano in questo momento; la voce arriva senza effetti. Puoi spegnerli in Ambiente.", "Walls and reverb aren't working right now; voices arrive without effects. You can turn them off in Environment."),
        _ => "",
    }
}

/// Records a problem (or a milestone) for the diagnostics tab and the log.
pub fn report(level: Level, phase: &str, code: &str, detail: impl Into<String>) {
    report_with_hint(level, phase, code, detail, hint(code))
}

pub fn report_with_hint(level: Level, phase: &str, code: &str, detail: impl Into<String>, hint: &str) {
    let detail: String = detail.into().chars().filter(|c| !c.is_control()).take(300).collect();
    let mut events = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = events.iter_mut().find(|e| e.code == code && e.phase == phase) {
        if last.detail == detail {
            last.count += 1;
            last.time = clock();
            return;
        }
    }
    log_info(format_args!("[{}] {phase}: {detail}", level.name()));
    events.retain(|e| !(e.code == code && e.phase == phase));
    events.push_back(Event {
        time: clock(),
        level,
        phase: phase.into(),
        code: code.into(),
        detail,
        hint: hint.into(),
        count: 1,
    });
    while events.len() > MAX {
        events.pop_front();
    }
}

/// Removes a problem that has been solved (e.g. "host not found" once found).
pub fn resolve(code: &str) {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|e| e.code != code);
}

/// `time \t level \t phase \t detail \t hint \t count`, newest first.
pub fn lines() -> Vec<String> {
    EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .map(|e| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                e.time,
                e.level.name(),
                e.phase,
                e.detail,
                e.hint,
                e.count
            )
        })
        .collect()
}

pub fn clear() {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
