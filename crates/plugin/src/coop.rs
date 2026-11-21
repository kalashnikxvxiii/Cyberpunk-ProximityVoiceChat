//! NightCoop through its SDK: found and registered from the game thread, then
//! the voice chat ([`vc_coop::chat`]) runs in NightCoop's frame. This side
//! only moves things between it and [`Shared`]: our pose, the partners'
//! bodies, frames to and from the audio thread, and the session for the UI.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use vc_coop::chat::{self, Chat, Frame, Game, Heard, Status};
use vc_coop::sdk::{self, Registration};
use vc_coop::{ModInfo, ffi};
use vc_protocol::{RosterEntry, Vec3};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::config::Mode;
use crate::events::{self, Level};
use crate::state::{Coop, Link, Shared, SpeakerInfo};
use crate::{keys, tr};

const LOOK_EVERY: Duration = Duration::from_secs(1);

static INFO: ModInfo = ModInfo {
    // Stable forever: NightCoop pairs mods by it.
    id: "kalashnikxv.voicechat",
    name: "VoiceChat",
    version: env!("CARGO_PKG_VERSION"),
    protocol: chat::PROTOCOL,
    flags: ffi::NCSDK_MOD_USES_CHANNELS | ffi::NCSDK_MOD_USES_SHARED_STATE,
};

thread_local! {
    static REG: RefCell<Option<Registration<Chat<Glue>>>> = const { RefCell::new(None) };
    static LAST_LOOK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// From the game thread's update: find NightCoop and register, retrying every second.
pub fn tick(shared: &Arc<Shared>) {
    REG.with_borrow_mut(|reg| {
        if reg.as_ref().is_some_and(Registration::alive) {
            return;
        }
        if reg.take().is_some() {
            set(shared, Coop::NoSdk(tr!("NightCoop si è chiuso", "NightCoop shut down")));
        }
        if LAST_LOOK.get().is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return;
        }
        LAST_LOOK.set(Some(Instant::now()));
        match find() {
            Err(state) => set(shared, state),
            Ok(api) => {
                // SAFETY: a table NightCoop returned for version 1; we stay on the game thread.
                match unsafe { sdk::register(api, &INFO, Chat::new(Glue { shared: shared.clone(), keys: String::new(), clash: String::new() })) } {
                    Ok(r) => {
                        let build = r
                            .with(|c, s| {
                                c.game.keys = s.keys_used();
                                s.build()
                            })
                            .unwrap_or_default();
                        crate::log_info(format_args!("nightcoop: registered with the SDK (build {build})"));
                        // Which keys it reads, by code: to check the push-to-talk warning against.
                        let keys = r.with(|_, s| s.keys_used()).unwrap_or_default();
                        crate::log_info(format_args!("nightcoop: keys it reads: {}", keys.trim_end().replace('\n', " | ")));
                        *shared.lock(&shared.nc_build) = build;
                        events::resolve("nc_sdk_off");
                        events::resolve("nc_sdk_refused");
                        *reg = Some(r);
                    }
                    Err(_) => set(shared, Coop::Refused),
                }
            }
        }
    });
}

/// Unregisters (the plugin is unloading).
pub fn stop() {
    REG.with_borrow_mut(|reg| drop(reg.take()));
}

fn set(shared: &Shared, state: Coop) {
    *shared.lock(&shared.coop) = state;
}

fn find() -> Result<*const ffi::NcSdkApi, Coop> {
    let name: Vec<u16> = "NightCoop.dll".encode_utf16().chain([0]).collect();
    // SAFETY: a zero-terminated name; the handle is only used for GetProcAddress.
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    if module.is_null() {
        return Err(Coop::Absent);
    }
    // SAFETY: as above, with a zero-terminated export name.
    let Some(get) = (unsafe { GetProcAddress(module, ffi::NCSDK_EXPORT_NAME.as_ptr()) }) else {
        return Err(Coop::NoSdk(tr!("questa versione di NightCoop non ha l'SDK: aggiornala", "this NightCoop has no SDK: update it")));
    };
    // SAFETY: NightCoopSdkGet's signature, from the SDK header.
    let get: unsafe extern "C" fn(u32) -> *const ffi::NcSdkApi = unsafe { std::mem::transmute(get) };
    // SAFETY: as above.
    let api = unsafe { get(ffi::NCSDK_VERSION) };
    if api.is_null() {
        return Err(Coop::NoSdk(tr!(
            "NightCoop non offre l'SDK (spento nelle sue impostazioni, o versione diversa): riattivalo o aggiorna NightCoop",
            "NightCoop doesn't offer the SDK (switched off in its settings, or another version): turn it back on or update NightCoop"
        )));
    }
    Ok(api)
}

/// The chat's view of the game and the audio thread.
pub struct Glue {
    shared: Arc<Shared>,
    /// [`sdk::Sdk::keys_used`], read once at registration.
    keys: String,
    /// What of NightCoop's our push-to-talk key also triggers, as reported.
    clash: String,
}

impl Glue {
    fn voice_here(&self) -> bool {
        self.shared.coop_voice.load(Ordering::Relaxed)
    }

    /// NightCoop doesn't consume its keys: pressing ours would also do what it does with it.
    fn check_ptt(&mut self) {
        let set = self.shared.settings();
        let clash =
            if set.mode == Mode::PushToTalk { sdk::key_users(&self.keys, set.ptt_vk).join(", ") } else { String::new() };
        if clash == self.clash {
            return;
        }
        if clash.is_empty() {
            events::resolve("ptt_nightcoop");
        } else {
            events::report(
                Level::Warn,
                "Audio",
                "ptt_nightcoop",
                tr!("il tasto per parlare ({}) lo usa anche NightCoop: {}", "the push-to-talk key ({}) is also NightCoop's: {}", keys::name(set.ptt_vk), clash),
            );
        }
        self.clash.clone_from(&clash);
        *self.shared.lock(&self.shared.ptt_clash) = clash;
    }
}

impl Game for Glue {
    fn me(&self) -> Option<Vec3> {
        self.shared.lock(&self.shared.pose).map(|p| p.pos())
    }

    fn body(&self, entity: u64) -> Option<Vec3> {
        crate::game::entity_position(entity).map_err(|e| crate::log_once("entity_position", &e)).ok().flatten()
    }

    fn party(&self) -> String {
        self.shared.settings().party
    }

    fn take_frames(&mut self, out: &mut VecDeque<Frame>) {
        let mut q = self.shared.lock(&self.shared.outbox);
        if self.voice_here() {
            out.extend(q.drain(..));
        } else {
            q.clear();
        }
    }

    fn heard(&mut self, h: Heard) {
        let Ok(from) = u16::try_from(h.from) else { return };
        if !self.voice_here() {
            return;
        }
        let now = Instant::now();
        let s = &self.shared;
        s.lock(&s.speakers)
            .entry(from)
            .and_modify(|i| {
                i.pos = h.pos;
                i.party = h.party;
                i.env = h.env;
                i.last_packet = now;
            })
            .or_insert(SpeakerInfo { pos: h.pos, party: h.party, env: h.env, last_packet: now, level_db: -100.0, audible_until: None });
        s.lock(&s.inbox).push((from, h.seq, h.opus, h.reset));
    }

    fn status(&mut self, st: &Status) {
        self.check_ptt();
        let s = &self.shared;
        let was_denied = matches!(&*s.lock(&s.coop), Coop::Ready(old) if old.denied);
        if st.denied && !was_denied {
            events::report(Level::Warn, "Session", "nc_denied", tr!("l'host ha spento le mod per questa sessione", "the host turned mods off for this session"));
        } else if !st.denied && was_denied {
            events::resolve("nc_denied");
        }
        set(s, Coop::Ready(st.clone()));
        // Under the link's lock: the session thread turns `coop_voice` off before it takes it, so
        // nothing written here outlives the plan.
        let mut l = s.lock(&s.link);
        if !self.voice_here() {
            return;
        }
        let roster: Vec<RosterEntry> = st
            .members
            .iter()
            .filter_map(|m| Some(RosterEntry { id: u16::try_from(m.peer).ok()?, name: m.name.clone(), party: m.party.clone() }))
            .collect();
        s.lock(&s.speakers).retain(|id, _| roster.iter().any(|p| p.id == *id));
        *s.lock(&s.roster) = roster;
        let link = match u16::try_from(st.me) {
            Ok(id) if st.ready && st.in_session => Link::Joined { id, range: chat::RANGE },
            _ => Link::Connecting,
        };
        if *l != link {
            if link.joined().is_some() {
                events::report(Level::Info, "Session", "connected", tr!("chat vocale attiva tramite NightCoop", "voice chat on, through NightCoop"));
            }
            *l = link;
        }
    }
}
