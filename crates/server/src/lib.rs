//! Voice relay. Clients join a room; voice is forwarded to everyone in the
//! same room who is within range of the speaker, or in the same party, or to
//! everyone when either side is outside the world (menus, the co-op lobby).
//!
//! Used by the standalone `vc-server` and, embedded, by the NightCoop host's
//! game.
//!
//! Trust model: the room (and, for NightCoop sessions, the pre-shared key) is
//! the access control. Inside a room, positions and party names are reported
//! by the clients themselves and not verified. Voice payloads are sealed end
//! to end, so the relay can route them but not listen to them.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vc_protocol::crypto::{self, KeyPair, Session};
use vc_protocol::{
    ClientMsg, Env, MAX_PACKET, MAX_ROSTER, PlayerId, RosterEntry, ServerMsg, VERSION,
    Vec3, clean_name, handshake,
};

const TIMEOUT: Duration = Duration::from_secs(10);
/// A handshake that never sends Hello is dropped sooner.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PER_ROOM: usize = MAX_ROSTER;
const MAX_SESSIONS: usize = 512;
const MAX_PER_IP: usize = 8;
const PUNCH_EVERY: Duration = Duration::from_millis(300);
const PUNCH_FOR: Duration = Duration::from_secs(15);

pub struct Config {
    pub range: f32,
    /// Mixed into every session's keys; all zeros for an open server.
    pub psk: [u8; 32],
    /// Joined players allowed in total; `usize::MAX` for no limit. The
    /// NightCoop host sets it to the session's player count, so nobody
    /// outside the session can take a seat.
    pub max_players: Arc<AtomicUsize>,
}

impl Default for Config {
    fn default() -> Self {
        Self { range: vc_protocol::RANGE, psk: [0; 32], max_players: Arc::new(AtomicUsize::new(usize::MAX)) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

/// Addresses to open our NAT towards (hole punching), each for a while.
#[derive(Default, Clone)]
pub struct Punches(Arc<Mutex<HashMap<SocketAddr, Instant>>>);

impl Punches {
    pub fn add(&self, addr: SocketAddr) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(addr, Instant::now() + PUNCH_FOR);
    }
}

/// A joined player.
struct Client {
    id: PlayerId,
    room: String,
    name: String,
    party: String,
    pos: Option<Vec3>,
}

struct Peer {
    session: Session,
    /// A new handshake from an address that already has a session. It only
    /// replaces the session once a packet sealed with it arrives, and even
    /// then the player must join again: a handshake proves nothing about who
    /// owned the old session, so it never inherits it.
    pending: Option<Session>,
    last_seen: Instant,
    client: Option<Client>,
}

pub struct Relay {
    sock: UdpSocket,
    cfg: Config,
    key: KeyPair,
    peers: HashMap<SocketAddr, Peer>,
    next_id: PlayerId,
    plain: Vec<u8>,
    sealed: Vec<u8>,
    log: Box<dyn Fn(Level, String) + Send>,
    punches: Punches,
}

fn dist2(a: Vec3, b: Vec3) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

impl Relay {
    pub fn new(sock: UdpSocket, key: KeyPair, cfg: Config, log: Box<dyn Fn(Level, String) + Send>) -> Self {
        Self {
            sock,
            cfg,
            key,
            peers: HashMap::new(),
            next_id: 1,
            plain: Vec::new(),
            sealed: Vec::new(),
            log,
            punches: Punches::default(),
        }
    }

    pub fn punches(&self) -> Punches {
        self.punches.clone()
    }

    fn log(&self, args: std::fmt::Arguments<'_>) {
        (self.log)(Level::Info, args.to_string());
    }

    fn warn(&self, args: std::fmt::Arguments<'_>) {
        (self.log)(Level::Warn, args.to_string());
    }

    /// Serves until `stop` is set. The socket should have a short read timeout.
    pub fn run(&mut self, stop: &AtomicBool) {
        let mut packet = [0u8; MAX_PACKET + crypto::OVERHEAD + 1];
        let mut last_sweep = Instant::now();
        let mut last_punch = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            // Errors: timeouts, and on Windows ICMP "port unreachable" from earlier sends.
            if let Ok((n, from)) = self.sock.recv_from(&mut packet)
                && n <= MAX_PACKET + crypto::OVERHEAD
            {
                self.datagram(from, &packet[..n]);
            }
            if last_sweep.elapsed() >= Duration::from_secs(1) {
                last_sweep = Instant::now();
                self.sweep();
            }
            if last_punch.elapsed() >= PUNCH_EVERY {
                last_punch = Instant::now();
                self.punch();
            }
        }
    }

    fn punch(&self) {
        let now = Instant::now();
        let mut punches = self.punches.0.lock().unwrap_or_else(|e| e.into_inner());
        punches.retain(|_, until| now < *until);
        for addr in punches.keys() {
            // Already talking to it: the path is open.
            if !self.peers.contains_key(addr) {
                let _ = self.sock.send_to(&[handshake::PUNCH], addr);
            }
        }
    }

    fn datagram(&mut self, from: SocketAddr, pkt: &[u8]) {
        match pkt.first() {
            Some(&handshake::HELLO) => self.handshake(from, pkt),
            Some(&crypto::SEALED) => {
                let Some(peer) = self.peers.get_mut(&from) else {
                    return self.raw(from, &[handshake::RESET]);
                };
                if peer.session.open(pkt, &mut self.plain).is_none() {
                    let Some(mut pending) = peer.pending.take() else { return };
                    if pending.open(pkt, &mut self.plain).is_none() {
                        peer.pending = Some(pending);
                        return;
                    }
                    peer.session = pending;
                    if let Some(c) = peer.client.take() {
                        self.log(format_args!("[{}] {:?} reconnected", c.room, c.name));
                        let room = c.room.clone();
                        self.broadcast_roster(&room);
                    }
                }
                let Some(peer) = self.peers.get_mut(&from) else { return };
                peer.last_seen = Instant::now();
                if let Some(msg) = ClientMsg::decode(&self.plain) {
                    self.handle(from, msg);
                }
            }
            _ => {}
        }
    }

    fn raw(&self, to: SocketAddr, bytes: &[u8]) {
        let _ = self.sock.send_to(bytes, to);
    }

    fn reject(&self, to: SocketAddr, reason: &str) {
        let mut out = vec![handshake::REJECT, reason.len() as u8];
        out.extend_from_slice(reason.as_bytes());
        self.raw(to, &out);
    }

    fn handshake(&mut self, from: SocketAddr, pkt: &[u8]) {
        if pkt.len() < handshake::HELLO_LEN {
            return;
        }
        let version = u16::from_le_bytes([pkt[1], pkt[2]]);
        if version != VERSION {
            return self.reject(from, &format!("protocol {version}, server wants {VERSION}"));
        }
        if !self.peers.contains_key(&from) {
            if self.peers.len() >= MAX_SESSIONS {
                return self.reject(from, "server full");
            }
            if self.peers.keys().filter(|a| a.ip() == from.ip()).count() >= MAX_PER_IP {
                return self.reject(from, "too many connections from this address");
            }
        }
        let client_pub: [u8; 32] = pkt[3..35].try_into().unwrap();
        let nonce: [u8; 32] = crypto::random_bytes();
        let mut session = Session::server(&self.key, &client_pub, &nonce, &self.cfg.psk);
        // Proof that we own the key the client is about to trust.
        session.seal(&[], &mut self.sealed);
        let mut out = Vec::with_capacity(1 + 64 + self.sealed.len());
        out.push(handshake::OK);
        out.extend_from_slice(&self.key.public);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&self.sealed);
        self.raw(from, &out);
        match self.peers.get_mut(&from) {
            Some(peer) => peer.pending = Some(session),
            None => {
                let peer = Peer { session, pending: None, last_seen: Instant::now(), client: None };
                self.peers.insert(from, peer);
            }
        }
    }

    fn send(&mut self, to: SocketAddr, msg: &ServerMsg) {
        let Some(peer) = self.peers.get_mut(&to) else { return };
        msg.encode(&mut self.plain);
        peer.session.seal(&self.plain, &mut self.sealed);
        let _ = self.sock.send_to(&self.sealed, to);
    }

    fn handle(&mut self, from: SocketAddr, msg: ClientMsg) {
        if let ClientMsg::Hello { version, room, name } = msg {
            return self.hello(from, version, room, name);
        }
        let Some(c) = self.peers.get_mut(&from).and_then(|p| p.client.as_mut()) else {
            return self.send(from, &ServerMsg::Reject { reason: "not joined".into() });
        };
        match msg {
            ClientMsg::Hello { .. } => unreachable!(),
            ClientMsg::Pose { pos } => c.pos = pos,
            ClientMsg::Voice { seq, pos, env, opus } => {
                c.pos = pos;
                self.relay(from, seq, pos, env, opus);
            }
            ClientMsg::Party { name } => {
                let name = clean_name(&name);
                if name != c.party {
                    let msg = format!("[{}] {} party -> {name:?}", c.room, c.name);
                    (self.log)(Level::Info, msg);
                    c.party = name;
                    let room = c.room.clone();
                    self.broadcast_roster(&room);
                }
            }
            ClientMsg::Bye => self.remove(from, "left"),
        }
    }

    fn hello(&mut self, from: SocketAddr, version: u16, room: String, name: String) {
        if version != VERSION {
            let reason = format!("protocol {version}, server wants {VERSION}");
            return self.send(from, &ServerMsg::Reject { reason });
        }
        let range = self.cfg.range;
        if let Some(c) = self.peers.get(&from).and_then(|p| p.client.as_ref()) {
            // Keepalive, or our Welcome was lost: answer again.
            let id = c.id;
            return self.send(from, &ServerMsg::Welcome { id, range });
        }
        let (room, name) = (clean_name(&room), clean_name(&name));
        let name = if name.is_empty() { "V".to_owned() } else { name };
        if self.clients().filter(|(_, c)| c.room == room).count() >= MAX_PER_ROOM {
            return self.send(from, &ServerMsg::Reject { reason: "room full".into() });
        }
        let joined = self.clients().count();
        if joined >= self.cfg.max_players.load(Ordering::Relaxed) {
            self.warn(format_args!("{name:?} from {from} refused: all {joined} seats of the session are taken"));
            return self.send(from, &ServerMsg::Reject { reason: "session full".into() });
        }
        let Some(id) = self.alloc_id() else {
            return self.send(from, &ServerMsg::Reject { reason: "server full".into() });
        };
        self.log(format_args!("[{room}] {name:?} joined from {from} as #{id}"));
        if let Some(peer) = self.peers.get_mut(&from) {
            peer.client = Some(Client {
                id,
                room: room.clone(),
                name,
                party: String::new(),
                pos: None,
            });
        }
        self.send(from, &ServerMsg::Welcome { id, range });
        self.broadcast_roster(&room);
    }

    fn clients(&self) -> impl Iterator<Item = (&SocketAddr, &Client)> {
        self.peers.iter().filter_map(|(a, p)| Some((a, p.client.as_ref()?)))
    }

    /// `None` when every id is taken (sessions are capped well below that).
    fn alloc_id(&mut self) -> Option<PlayerId> {
        for _ in 0..PlayerId::MAX {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.clients().any(|(_, c)| c.id == id) {
                return Some(id);
            }
        }
        None
    }

    fn relay(&mut self, from: SocketAddr, seq: u32, pos: Option<Vec3>, env: Env, opus: Vec<u8>) {
        let Some(speaker) = self.peers.get(&from).and_then(|p| p.client.as_ref()) else { return };
        // Shouting carries further.
        // As far as a voice fades out, past its range.
        let range = self.cfg.range * vc_protocol::FADE_TAIL * env.reach();
        let range2 = range * range;
        let targets: Vec<(SocketAddr, bool)> = self
            .clients()
            .filter(|(addr, c)| **addr != from && c.room == speaker.room)
            .filter_map(|(addr, c)| {
                let party = !speaker.party.is_empty() && c.party == speaker.party;
                // Outside the world (menus, lobby) everybody hears everybody.
                let near = match (c.pos, pos) {
                    (Some(a), Some(b)) => dist2(a, b) <= range2,
                    _ => true,
                };
                (party || near).then_some((*addr, party))
            })
            .collect();
        let id = speaker.id;
        for (to, party) in targets {
            let msg = ServerMsg::Voice { from: id, party, seq, pos, env, opus: opus.clone() };
            self.send(to, &msg);
        }
    }

    fn broadcast_roster(&mut self, room: &str) {
        let mut members: Vec<(SocketAddr, RosterEntry)> = self
            .clients()
            .filter(|(_, c)| c.room == room)
            .map(|(a, c)| (*a, RosterEntry { id: c.id, name: c.name.clone(), party: c.party.clone() }))
            .collect();
        members.sort_by_key(|(_, e)| e.id);
        let msg = ServerMsg::Roster { players: members.iter().map(|(_, e)| e.clone()).collect() };
        for (addr, _) in members {
            self.send(addr, &msg);
        }
    }

    fn remove(&mut self, addr: SocketAddr, why: &str) {
        if let Some(c) = self.peers.remove(&addr).and_then(|p| p.client) {
            self.log(format_args!("[{}] {:?} {why}", c.room, c.name));
            self.broadcast_roster(&c.room);
        }
    }

    fn sweep(&mut self) {
        let stale: Vec<SocketAddr> = self
            .peers
            .iter()
            .filter(|(_, p)| {
                let limit = if p.client.is_some() { TIMEOUT } else { HANDSHAKE_TIMEOUT };
                p.last_seen.elapsed() > limit
            })
            .map(|(a, _)| *a)
            .collect();
        for addr in stale {
            self.remove(addr, "timed out");
        }
    }
}
