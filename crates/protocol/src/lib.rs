//! Wire format shared by the plugin and the server.
//!
//! Every datagram starts with a one-byte kind. Integers and floats are
//! little-endian; strings are a `u8` length followed by UTF-8 bytes.
//! Voice packets end with the raw Opus payload.
//!
//! Only the handshake travels in clear; [`ClientMsg`] and [`ServerMsg`] are
//! always wrapped by [`crypto::Session`]. Voice payloads are additionally
//! sealed end to end ([`e2e`]): servers route them without being able to
//! listen.
//!
//! Positions are optional: `None` means "not in the world" (menus, the
//! NightCoop lobby), where everybody hears everybody without distance.

pub mod crypto;
pub mod e2e;

pub const VERSION: u16 = 7;
/// How far a usual voice carries in the open, in the game's metres (see vc_dsp's `NEAR`).
pub const RANGE: f32 = 72.0;
/// Past its range a voice fades out over this much further again (see
/// `vc_dsp::spatial::FADE_TAIL`): sent and accepted that far.
pub const FADE_TAIL: f32 = 2.5;
pub const DEFAULT_PORT: u16 = 47900;
/// Larger datagrams are dropped; keeps us under typical MTUs.
pub const MAX_PACKET: usize = 1200;

pub type Vec3 = [f32; 3];
pub type PlayerId = u16;

/// Names and party names are cut to this many bytes by the server.
pub const MAX_NAME: usize = 32;
/// Players listed in one roster; keeps the datagram under [`MAX_PACKET`].
pub const MAX_ROSTER: usize = 16;

/// Reverb of the speaker's surroundings, so listeners hear the same room.
/// `wet` 0 = open air, 255 = very reverberant; `size` 0 = small, 255 = huge.
/// `loud`: dB above the speaker's usual voice (shouting carries further).
/// `ear`: the speaker's ears above their feet, cm (crouching, seated); 0 = not known.
/// `damp`: how soft their room's surfaces are (highs absorbed), 0..255.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Env {
    pub wet: u8,
    pub size: u8,
    pub loud: u8,
    pub ear: u8,
    pub damp: u8,
}

/// The loudest shout counted, dB over the usual voice: 10x the range.
/// Real lungs top out about here, and so does a microphone before clipping.
pub const LOUD_MAX: u8 = 20;

impl Env {
    /// How much further than usual this voice carries: +6 dB, twice as far.
    pub fn reach(&self) -> f32 {
        10f32.powf(f32::from(self.loud.min(LOUD_MAX)) / 20.0)
    }

    /// The speaker's ears above their feet (m), if they told.
    pub fn ear_m(&self) -> Option<f32> {
        (self.ear > 0).then(|| f32::from(self.ear) / 100.0)
    }

    /// For [`Env::ear`]: metres to cm, 0 kept for "not known".
    pub fn ear_cm(m: f32) -> u8 {
        (m * 100.0).round().clamp(1.0, 255.0) as u8
    }
}

/// Plaintext handshake datagrams.
pub mod handshake {
    /// Client → server: `[HELLO][version u16][ephemeral key 32][padding]`.
    /// Padded to at least the size of the answer, so the server can't be
    /// used to amplify spoofed traffic.
    pub const HELLO: u8 = 0x10;
    pub const HELLO_LEN: usize = 96;
    /// Server → client: `[OK][server key 32][nonce 32][sealed empty message]`.
    pub const OK: u8 = 0x90;
    /// Server → client: `[REJECT][reason str]` (version mismatch, server full).
    pub const REJECT: u8 = 0x91;
    /// Server → client: unknown session, handshake again.
    pub const RESET: u8 = 0x92;
    /// Server → client, unrequested: opens the server's NAT towards a client
    /// that is about to connect (hole punching). Clients ignore it. Only sent
    /// to addresses from authenticated session records.
    pub const PUNCH: u8 = 0x93;

    use crate::crypto::{KeyPair, Session};

    pub fn hello(ephemeral: &KeyPair) -> Vec<u8> {
        let mut out = vec![0u8; HELLO_LEN];
        out[0] = HELLO;
        out[1..3].copy_from_slice(&crate::VERSION.to_le_bytes());
        out[3..35].copy_from_slice(&ephemeral.public);
        out
    }

    /// Checks the server's answer; returns the session and the server key.
    /// `psk` must match the server's (all zeros for an open server).
    pub fn accept(ephemeral: &KeyPair, pkt: &[u8], psk: &[u8; 32]) -> Option<(Session, [u8; 32])> {
        if pkt.first() != Some(&OK) || pkt.len() < 65 {
            return None;
        }
        let server_pub: [u8; 32] = pkt[1..33].try_into().ok()?;
        let nonce: [u8; 32] = pkt[33..65].try_into().ok()?;
        let mut session = Session::client(ephemeral, &server_pub, &nonce, psk);
        let mut proof = Vec::new();
        session.open(&pkt[65..], &mut proof)?;
        proof.is_empty().then_some((session, server_pub))
    }

    /// Reason text of a `REJECT`.
    pub fn reject_reason(pkt: &[u8]) -> Option<String> {
        if pkt.first() != Some(&REJECT) {
            return None;
        }
        let len = *pkt.get(1)? as usize;
        let bytes = pkt.get(2..2 + len)?;
        Some(crate::clean_name(&String::from_utf8_lossy(bytes)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMsg {
    Hello { version: u16, room: String, name: String },
    Pose { pos: Option<Vec3> },
    /// `opus` is sealed with [`e2e::VoiceKey`].
    Voice { seq: u32, pos: Option<Vec3>, env: Env, opus: Vec<u8> },
    /// Empty name leaves the current party.
    Party { name: String },
    Bye,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RosterEntry {
    pub id: PlayerId,
    pub name: String,
    pub party: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMsg {
    /// `range`: proximity radius in metres, decided by the server.
    Welcome { id: PlayerId, range: f32 },
    Reject { reason: String },
    Voice { from: PlayerId, party: bool, seq: u32, pos: Option<Vec3>, env: Env, opus: Vec<u8> },
    Roster { players: Vec<RosterEntry> },
}

mod kind {
    pub const HELLO: u8 = 0x01;
    pub const POSE: u8 = 0x02;
    pub const VOICE: u8 = 0x03;
    pub const PARTY: u8 = 0x04;
    pub const BYE: u8 = 0x05;
    pub const WELCOME: u8 = 0x81;
    pub const REJECT: u8 = 0x82;
    pub const S_VOICE: u8 = 0x83;
    pub const ROSTER: u8 = 0x84;
}

impl ClientMsg {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        let mut w = Writer(out);
        match self {
            Self::Hello { version, room, name } => {
                w.u8(kind::HELLO);
                w.u16(*version);
                w.str(room);
                w.str(name);
            }
            Self::Pose { pos } => {
                w.u8(kind::POSE);
                w.opt_vec3(pos);
            }
            Self::Voice { seq, pos, env, opus } => {
                w.u8(kind::VOICE);
                w.u32(*seq);
                w.opt_vec3(pos);
                w.env(env);
                w.0.extend_from_slice(opus);
            }
            Self::Party { name } => {
                w.u8(kind::PARTY);
                w.str(name);
            }
            Self::Bye => w.u8(kind::BYE),
        }
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut r = Reader(buf);
        Some(match r.u8()? {
            kind::HELLO => Self::Hello { version: r.u16()?, room: r.str()?, name: r.str()? },
            kind::POSE => Self::Pose { pos: r.opt_vec3()? },
            kind::VOICE => Self::Voice { seq: r.u32()?, pos: r.opt_vec3()?, env: r.env()?, opus: r.rest() },
            kind::PARTY => Self::Party { name: r.str()? },
            kind::BYE => Self::Bye,
            _ => return None,
        })
    }
}

impl ServerMsg {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        let mut w = Writer(out);
        match self {
            Self::Welcome { id, range } => {
                w.u8(kind::WELCOME);
                w.u16(*id);
                w.0.extend_from_slice(&range.to_le_bytes());
            }
            Self::Reject { reason } => {
                w.u8(kind::REJECT);
                w.str(reason);
            }
            Self::Voice { from, party, seq, pos, env, opus } => {
                w.u8(kind::S_VOICE);
                w.u16(*from);
                w.u8(*party as u8);
                w.u32(*seq);
                w.opt_vec3(pos);
                w.env(env);
                w.0.extend_from_slice(opus);
            }
            Self::Roster { players } => {
                w.u8(kind::ROSTER);
                w.u8(players.len().min(MAX_ROSTER) as u8);
                for p in players.iter().take(MAX_ROSTER) {
                    w.u16(p.id);
                    w.str(&p.name);
                    w.str(&p.party);
                }
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut r = Reader(buf);
        Some(match r.u8()? {
            kind::WELCOME => Self::Welcome { id: r.u16()?, range: r.f32()? },
            kind::REJECT => Self::Reject { reason: r.str()? },
            kind::S_VOICE => Self::Voice {
                from: r.u16()?,
                party: r.u8()? != 0,
                seq: r.u32()?,
                pos: r.opt_vec3()?,
                env: r.env()?,
                opus: r.rest(),
            },
            kind::ROSTER => {
                let n = r.u8()?;
                let players = (0..n)
                    .map(|_| Some(RosterEntry { id: r.u16()?, name: r.str()?, party: r.str()? }))
                    .collect::<Option<_>>()?;
                Self::Roster { players }
            }
            _ => return None,
        })
    }
}

/// Trims to at most 255 bytes on a char boundary.
pub fn clamp_str(s: &str) -> &str {
    clamp_bytes(s, u8::MAX as usize)
}

fn clamp_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Display-safe player or party name: no control characters, trimmed,
/// at most [`MAX_NAME`] bytes.
pub fn clean_name(s: &str) -> String {
    let no_ctrl: String = s.chars().filter(|c| !c.is_control()).collect();
    clamp_bytes(no_ctrl.trim(), MAX_NAME).trim_end().to_owned()
}

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn vec3(&mut self, v: &Vec3) {
        for c in v {
            self.0.extend_from_slice(&c.to_le_bytes());
        }
    }
    fn opt_vec3(&mut self, v: &Option<Vec3>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.vec3(v);
            }
            None => self.u8(0),
        }
    }
    fn env(&mut self, e: &Env) {
        self.0.extend_from_slice(&[e.wet, e.size, e.loud, e.ear, e.damp]);
    }
    fn str(&mut self, s: &str) {
        let s = clamp_str(s);
        self.u8(s.len() as u8);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.0.split_first_chunk::<N>()?;
        self.0 = tail;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take().map(f32::from_le_bytes).filter(|v: &f32| v.is_finite())
    }
    fn vec3(&mut self) -> Option<Vec3> {
        Some([self.f32()?, self.f32()?, self.f32()?])
    }
    fn opt_vec3(&mut self) -> Option<Option<Vec3>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.vec3()?)),
            _ => None,
        }
    }
    fn env(&mut self) -> Option<Env> {
        let [wet, size, loud, ear, damp] = self.take()?;
        Some(Env { wet, size, loud, ear, damp })
    }
    fn str(&mut self) -> Option<String> {
        let len = self.u8()? as usize;
        let bytes = self.0.get(..len)?;
        self.0 = &self.0[len..];
        String::from_utf8(bytes.to_vec()).ok()
    }
    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_client(m: ClientMsg) {
        let mut buf = Vec::new();
        m.encode(&mut buf);
        assert_eq!(ClientMsg::decode(&buf), Some(m));
    }

    fn rt_server(m: ServerMsg) {
        let mut buf = Vec::new();
        m.encode(&mut buf);
        assert_eq!(ServerMsg::decode(&buf), Some(m));
    }

    #[test]
    fn roundtrip() {
        rt_client(ClientMsg::Hello { version: VERSION, room: "abc".into(), name: "Vè".into() });
        rt_client(ClientMsg::Pose { pos: Some([1.0, -2.5, 7.2]) });
        rt_client(ClientMsg::Pose { pos: None });
        rt_client(ClientMsg::Voice { seq: 42, pos: Some([0.0; 3]), env: Env { wet: 9, size: 200, loud: 7, ear: 104, damp: 80 }, opus: vec![1, 2, 3] });
        assert!((Env { loud: 6, ..Env::default() }.reach() - 2.0).abs() < 0.01);
        assert!((Env { loud: 255, ..Env::default() }.reach() - 10.0).abs() < 0.01);
        rt_client(ClientMsg::Voice { seq: 43, pos: None, env: Env::default(), opus: vec![4] });
        rt_client(ClientMsg::Party { name: String::new() });
        rt_client(ClientMsg::Bye);
        rt_server(ServerMsg::Welcome { id: 7, range: 48.0 });
        rt_server(ServerMsg::Reject { reason: "no".into() });
        rt_server(ServerMsg::Voice { from: 3, party: true, seq: 9, pos: Some([1.0; 3]), env: Env::default(), opus: vec![9] });
        rt_server(ServerMsg::Voice { from: 3, party: false, seq: 9, pos: None, env: Env::default(), opus: vec![] });
        rt_server(ServerMsg::Roster {
            players: vec![RosterEntry { id: 1, name: "a".into(), party: "p".into() }],
        });
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(ClientMsg::decode(&[]), None);
        assert_eq!(ClientMsg::decode(&[0xff]), None);
        assert_eq!(ClientMsg::decode(&[kind::HELLO, 1]), None);
        assert_eq!(ClientMsg::decode(&[kind::PARTY, 5, b'a']), None);
        let mut nan = vec![kind::POSE, 1];
        nan.extend_from_slice(&f32::NAN.to_le_bytes());
        nan.extend_from_slice(&[0; 8]);
        assert_eq!(ClientMsg::decode(&nan), None);
    }

    #[test]
    fn clean_name_strips_control_and_caps() {
        assert_eq!(clean_name(" a\u{1b}[31mb\n "), "a[31mb");
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME);
        assert_eq!(clean_name(&"é".repeat(40)).len(), 32);
    }

    #[test]
    fn long_strings_are_clamped() {
        let mut buf = Vec::new();
        ClientMsg::Party { name: "é".repeat(200) }.encode(&mut buf);
        let Some(ClientMsg::Party { name }) = ClientMsg::decode(&buf) else { panic!() };
        assert_eq!(name.len(), 254);
    }
}
