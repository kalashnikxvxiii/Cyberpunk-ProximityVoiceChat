//! NightCoop's SDK (C ABI, version 1) behind a thin trait, so the logic on top
//! of it runs the same over the real table ([`sdk`]) and over an in-memory
//! NightCoop with several machines ([`fake`], tests only).
//!
//! Everything here is for the game thread: [`Coop`] calls and [`Hooks`]
//! callbacks alike. Other threads queue their work and a hook drains it.

pub mod chat;
pub mod ffi;
pub mod sdk;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

/// A player of the session; the host is always [`HOST`].
pub type Peer = u32;

pub const HOST: Peer = ffi::NCSDK_PEER_HOST;
pub const NONE: Peer = ffi::NCSDK_PEER_NONE;
/// `send`: everybody in the session who has the mod.
pub const TO_ALL: Peer = ffi::NCSDK_TO_ALL;

pub use ffi::{
    NCSDK_E_ARG as E_ARG, NCSDK_E_DENIED as E_DENIED, NCSDK_E_LIMIT as E_LIMIT, NCSDK_E_OFF as E_OFF,
    NCSDK_E_SIZE as E_SIZE, NCSDK_E_STATE as E_STATE, NCSDK_E_UNKNOWN as E_UNKNOWN, NCSDK_OK as OK,
    NCSDK_RATE_BYTES_PER_S as RATE_BYTES_PER_S, NCSDK_RATE_MESSAGES_PER_S as RATE_MESSAGES_PER_S,
    NCSDK_RELIABLE_MAX as RELIABLE_MAX, NCSDK_SEND_RELIABLE as SEND_RELIABLE, NCSDK_STATE_KEY_MAX as STATE_KEY_MAX,
    NCSDK_STATE_VALUE_MAX as STATE_VALUE_MAX, NCSDK_UNRELIABLE_MAX as UNRELIABLE_MAX,
};

/// Session events (`hooks.event`), the `NCSDK_EV_*` numbers.
pub mod ev {
    use crate::ffi;
    pub const SESSION_START: i32 = ffi::NCSDK_EV_SESSION_START as i32;
    pub const SESSION_END: i32 = ffi::NCSDK_EV_SESSION_END as i32;
    pub const PEER_JOIN: i32 = ffi::NCSDK_EV_PEER_JOIN as i32;
    pub const PEER_LEAVE: i32 = ffi::NCSDK_EV_PEER_LEAVE as i32;
    pub const PEER_WORLD_IN: i32 = ffi::NCSDK_EV_PEER_WORLD_IN as i32;
    pub const PEER_WORLD_OUT: i32 = ffi::NCSDK_EV_PEER_WORLD_OUT as i32;
    pub const PEER_MOD: i32 = ffi::NCSDK_EV_PEER_MOD as i32;
    pub const PEER_NO_MOD: i32 = ffi::NCSDK_EV_PEER_NO_MOD as i32;
}

/// `where()`: the session as NightCoop sees it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Where {
    pub in_session: bool,
    pub is_host: bool,
    pub in_world: bool,
    pub in_host_world: bool,
    pub local_peer: Peer,
    pub peer_count: u32,
    /// The session clock, the same on every machine; 0 out of a session.
    pub shared_time_ms: u64,
}

impl Default for Where {
    fn default() -> Self {
        Self {
            in_session: false,
            is_host: false,
            in_world: false,
            in_host_world: false,
            local_peer: NONE,
            peer_count: 0,
            shared_time_ms: 0,
        }
    }
}

/// `partnerState()`: the flags (`NCSDK_PARTNER_*`) and where they are (`NCSDK_WHERE_*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartnerState {
    pub flags: u32,
    pub place: i32,
}

/// What the logic calls. Game thread only.
pub trait Coop {
    fn where_(&self) -> Where;
    /// The other players of the session.
    fn peers(&self) -> Vec<Peer>;
    /// The player's name as in NightCoop's player list, already through `clean_text`.
    fn peer_name(&self, peer: Peer) -> String;
    /// 1 has the mod (same id and protocol), 0 not, `E_STATE` not known yet.
    fn peer_has_mod(&self, peer: Peer) -> i32;
    /// Peers reached (0: nobody has the mod) or `E_*`.
    fn send(&self, to: Peer, channel: u16, data: &[u8], flags: u32) -> i32;
    /// Host: set and spread. Guest: ask the host. Empty `data` removes the key.
    fn state_set(&self, key: &str, data: &[u8]) -> i32;
    fn state_get(&self, key: &str) -> Option<Vec<u8>>;
    /// EntityID hash of the partner's body in our world, 0 if none now. Never keep it.
    fn partner_body(&self, peer: Peer) -> u64;
    fn partner_state(&self, peer: Peer) -> Option<PartnerState>;
    /// Safe to show or log: valid UTF-8, no control or invisible characters, at most `max` bytes.
    fn clean_text(&self, text: &[u8], max: usize) -> String;
    fn log(&self, level: i32, text: &str);
}

/// What NightCoop calls, from its frame on the game thread. Must not block.
pub trait Hooks {
    fn frame(&mut self, coop: &dyn Coop, dt: f32);
    fn event(&mut self, coop: &dyn Coop, event: i32, peer: Peer);
    fn message(&mut self, coop: &dyn Coop, from: Peer, channel: u16, data: &[u8]);
    /// `None`: the key was removed (also at the end of a session).
    fn state_changed(&mut self, coop: &dyn Coop, key: &str, data: Option<&[u8]>, by: Peer);
    /// Host only: may the guest `from` set this key?
    fn state_request(&mut self, coop: &dyn Coop, from: Peer, key: &str, data: &[u8]) -> bool;
    /// NightCoop is unloading: never call it again.
    fn shutdown(&mut self);
}

/// The passport we register with.
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub protocol: u32,
    pub flags: u32,
}
