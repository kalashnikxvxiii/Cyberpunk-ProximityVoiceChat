//! Wire format shared by the plugin and the server.
//!
//! Every datagram starts with a one-byte kind. Integers and floats are
//! little-endian; strings are a `u8` length followed by UTF-8 bytes.
//! Voice packets end with the raw Opus payload.

pub const VERSION: u16 = 1;
pub const DEFAULT_PORT: u16 = 47900;
/// Larger datagrams are dropped; keeps us under typical MTUs.
pub const MAX_PACKET: usize = 1200;

pub type Vec3 = [f32; 3];
pub type PlayerId = u16;

/// Sent by the server instead of a real id for the echo test bot.
pub const ECHO_ID: PlayerId = PlayerId::MAX;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMsg {
    Hello { version: u16, room: String, name: String },
    Pose { pos: Vec3 },
    Voice { seq: u32, pos: Vec3, opus: Vec<u8> },
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
    Voice { from: PlayerId, party: bool, seq: u32, pos: Vec3, opus: Vec<u8> },
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
                w.vec3(pos);
            }
            Self::Voice { seq, pos, opus } => {
                w.u8(kind::VOICE);
                w.u32(*seq);
                w.vec3(pos);
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
            kind::POSE => Self::Pose { pos: r.vec3()? },
            kind::VOICE => Self::Voice { seq: r.u32()?, pos: r.vec3()?, opus: r.rest() },
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
            Self::Voice { from, party, seq, pos, opus } => {
                w.u8(kind::S_VOICE);
                w.u16(*from);
                w.u8(*party as u8);
                w.u32(*seq);
                w.vec3(pos);
                w.0.extend_from_slice(opus);
            }
            Self::Roster { players } => {
                w.u8(kind::ROSTER);
                w.u8(players.len().min(u8::MAX as usize) as u8);
                for p in players.iter().take(u8::MAX as usize) {
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
                pos: r.vec3()?,
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
    let mut end = s.len().min(u8::MAX as usize);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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
        rt_client(ClientMsg::Pose { pos: [1.0, -2.5, 7.2] });
        rt_client(ClientMsg::Voice { seq: 42, pos: [0.0; 3], opus: vec![1, 2, 3] });
        rt_client(ClientMsg::Party { name: String::new() });
        rt_client(ClientMsg::Bye);
        rt_server(ServerMsg::Welcome { id: 7, range: 48.0 });
        rt_server(ServerMsg::Reject { reason: "no".into() });
        rt_server(ServerMsg::Voice { from: 3, party: true, seq: 9, pos: [1.0; 3], opus: vec![9] });
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
        let mut nan = vec![kind::POSE];
        nan.extend_from_slice(&f32::NAN.to_le_bytes());
        nan.extend_from_slice(&[0; 8]);
        assert_eq!(ClientMsg::decode(&nan), None);
    }

    #[test]
    fn long_strings_are_clamped() {
        let mut buf = Vec::new();
        ClientMsg::Party { name: "é".repeat(200) }.encode(&mut buf);
        let Some(ClientMsg::Party { name }) = ClientMsg::decode(&buf) else { panic!() };
        assert_eq!(name.len(), 254);
    }
}
