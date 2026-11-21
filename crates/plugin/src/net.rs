//! One encrypted connection to a voice server: the host's (direct), a relay,
//! or a manually configured one. The session manager starts and stops these.
//!
//! The socket is not connected: we send the handshake to every candidate
//! address of the server (LAN, public, router-mapped) and keep the first that
//! answers; from then on only that address is listened to.

use crate::tr;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vc_protocol::crypto::{self, KeyPair};
use vc_protocol::e2e::VoiceKey;
use vc_protocol::{ClientMsg, MAX_PACKET, ServerMsg, VERSION, handshake};

use crate::events::{self, Level};
use crate::log_info;
use crate::state::{Link, Shared, SpeakerInfo};

const RETRY_EVERY: Duration = Duration::from_millis(700);
const KEEPALIVE_EVERY: Duration = Duration::from_secs(1);
const POSE_EVERY: Duration = Duration::from_millis(200);
/// No answer for this long: the session is dead, start over.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(6);
/// A server whose key doesn't match the pinned one is retried this rarely.
const MISMATCH_RETRY: Duration = Duration::from_secs(30);

/// The encrypted channel, once the handshake is done.
pub struct Channel {
    sock: UdpSocket,
    peer: SocketAddr,
    session: crypto::Session,
    buf: Vec<u8>,
    plain: Vec<u8>,
}

impl Channel {
    pub fn send(&mut self, msg: &ClientMsg) {
        msg.encode(&mut self.plain);
        self.session.seal(&self.plain, &mut self.buf);
        let _ = self.sock.send_to(&self.buf, self.peer);
    }
}

/// Through this the audio thread sends voice; `None` while not connected.
#[derive(Default)]
pub struct ChannelSlot(Mutex<Option<Channel>>);

impl ChannelSlot {
    pub fn get(&self) -> MutexGuard<'_, Option<Channel>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn send(&self, msg: &ClientMsg) {
        if let Some(ch) = self.get().as_mut() {
            ch.send(msg);
        }
    }

    pub fn clear(&self) {
        *self.get() = None;
    }
}

/// How the server proves who it is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// An open server: pin its key on first use (`server_key` setting).
    PinInSettings,
    /// Pinned in the address (`host:port#FINGERPRINT`), e.g. the host's relay.
    Fingerprint,
}

pub struct Params {
    pub sock: UdpSocket,
    /// Candidate addresses of the server; may grow while connecting.
    pub targets: Arc<Mutex<Vec<SocketAddr>>>,
    pub psk: [u8; 32],
    pub trust: Trust,
    pub fingerprint: String,
    pub room: String,
    pub voice_key: VoiceKey,
    /// Shown in the UI: direct, relay, manual (already translated).
    pub via: &'static str,
    /// The phase name for diagnostics.
    pub phase: &'static str,
    pub stop: Arc<AtomicBool>,
}

pub fn run(shared: Arc<Shared>, slot: Arc<ChannelSlot>, p: Params) {
    *shared.lock(&shared.voice_key) = Some(p.voice_key.clone());
    let now = Instant::now();
    Client {
        shared,
        slot,
        p,
        ephemeral: None,
        last_attempt: None,
        last_heard: now,
        last_keepalive: now,
        last_pose: now,
        sent_party: None,
        retry_after: None,
        last_reject: None,
        decrypt_failures: 0,
    }
    .run();
}

struct Client {
    shared: Arc<Shared>,
    slot: Arc<ChannelSlot>,
    p: Params,
    /// Our half of a handshake in progress.
    ephemeral: Option<KeyPair>,
    last_attempt: Option<Instant>,
    last_heard: Instant,
    last_keepalive: Instant,
    last_pose: Instant,
    sent_party: Option<String>,
    retry_after: Option<Instant>,
    last_reject: Option<String>,
    decrypt_failures: u32,
}

impl Client {
    fn run(mut self) {
        let mut packet = [0u8; MAX_PACKET + crypto::OVERHEAD + 64];
        let mut plain = Vec::new();
        while !self.p.stop.load(Ordering::Relaxed) && !self.shared.stop.load(Ordering::Relaxed) {
            self.tick();
            if let Ok((n, from)) = self.p.sock.recv_from(&mut packet) {
                self.datagram(from, &packet[..n], &mut plain);
            }
        }
        self.slot.send(&ClientMsg::Bye);
        self.slot.clear();
    }

    fn set_link(&self, link: Link) {
        *self.shared.lock(&self.shared.link) = link;
    }

    fn joined(&self) -> bool {
        self.shared.lock(&self.shared.link).joined().is_some()
    }

    fn connected(&self) -> bool {
        self.slot.get().is_some()
    }

    fn reset(&mut self) {
        self.ephemeral = None;
        self.slot.clear();
        self.sent_party = None;
        self.set_link(Link::Connecting);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.retry_after.is_some_and(|t| now < t) {
            return;
        }
        if self.connected() && self.last_heard.elapsed() > SILENCE_TIMEOUT {
            events::report(Level::Warn, self.p.phase, "server_lost", tr!("il server ha smesso di rispondere, mi ricollego", "the server stopped answering, reconnecting"));
            self.reset();
        }
        if !self.connected() {
            if self.last_attempt.is_none_or(|t| t.elapsed() >= RETRY_EVERY) {
                self.last_attempt = Some(now);
                let eph = KeyPair::generate();
                let hello = handshake::hello(&eph);
                for t in self.p.targets.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                    let _ = self.p.sock.send_to(&hello, t);
                }
                self.ephemeral = Some(eph);
            }
            return;
        }
        let pose = *self.shared.lock(&self.shared.pose);
        // Hello until welcomed; afterwards it is the keepalive, answered with
        // Welcome, which tells us the server is still there.
        if self.last_keepalive.elapsed() >= KEEPALIVE_EVERY {
            self.last_keepalive = now;
            self.slot.send(&ClientMsg::Hello {
                version: VERSION,
                room: self.p.room.clone(),
                name: self.shared.name.clone(),
            });
        }
        if !self.joined() {
            return;
        }
        if self.last_pose.elapsed() >= POSE_EVERY {
            self.last_pose = now;
            // No pose = menus or the co-op lobby: heard by everybody.
            self.slot.send(&ClientMsg::Pose { pos: pose.map(|p| p.pos()) });
        }
        let party = self.shared.settings().party;
        let want = (!party.is_empty()).then_some(party);
        if want != self.sent_party {
            self.slot.send(&ClientMsg::Party { name: want.clone().unwrap_or_default() });
            self.sent_party = want;
        }
    }

    fn datagram(&mut self, from: SocketAddr, pkt: &[u8], plain: &mut Vec<u8>) {
        let peer = self.slot.get().as_ref().map(|c| c.peer);
        match peer {
            Some(p) if p != from => return,
            None if !self.p.targets.lock().unwrap_or_else(|e| e.into_inner()).contains(&from) => return,
            _ => {}
        }
        match pkt.first() {
            Some(&handshake::OK) => self.handshake_ok(from, pkt),
            Some(&handshake::REJECT) => {
                let reason = handshake::reject_reason(pkt).unwrap_or_default();
                self.rejected(reason);
            }
            Some(&handshake::RESET) => {
                if self.connected() {
                    events::report(Level::Info, self.p.phase, "server_reset", tr!("il server non ci conosceva più (riavviato?), mi ricollego", "the server no longer knew us (restarted?), reconnecting"));
                    self.reset();
                }
            }
            Some(&crypto::SEALED) => {
                let opened = match self.slot.get().as_mut() {
                    Some(ch) => ch.session.open(pkt, plain).is_some(),
                    None => false,
                };
                if !opened {
                    return;
                }
                self.last_heard = Instant::now();
                if let Some(msg) = ServerMsg::decode(plain) {
                    self.message(msg);
                }
            }
            _ => {}
        }
    }

    fn handshake_ok(&mut self, from: SocketAddr, pkt: &[u8]) {
        if self.connected() {
            return;
        }
        let Some(eph) = &self.ephemeral else { return };
        let Some((session, server_pub)) = handshake::accept(eph, pkt, &self.p.psk) else { return };
        let fp = crypto::fingerprint(&server_pub);
        match self.p.trust {
            Trust::Fingerprint => {
                if !self.p.fingerprint.eq_ignore_ascii_case(&fp) {
                    let msg = tr!("la chiave del server ({fp}) non corrisponde a quella indicata ({})", "the server's key ({fp}) doesn't match the given one ({})", self.p.fingerprint);
                    events::report(Level::Error, self.p.phase, "server_key_mismatch", msg.clone());
                    self.set_link(Link::Failed(msg));
                    self.retry_after = Some(Instant::now() + MISMATCH_RETRY);
                    return;
                }
            }
            Trust::PinInSettings => {
                let pinned = self.shared.settings().server_key;
                if pinned.is_empty() {
                    events::report(Level::Info, self.p.phase, "server_key_pinned", tr!("primo collegamento: salvata la chiave del server {fp}", "first connection: saved the server key {fp}"));
                    let _ = self.shared.update(|s| s.set("server_key", &fp));
                } else if !pinned.eq_ignore_ascii_case(&fp) {
                    let msg = tr!("la chiave del server ({fp}) non corrisponde a quella salvata ({pinned})", "the server's key ({fp}) doesn't match the saved one ({pinned})");
                    events::report(Level::Error, self.p.phase, "server_key_mismatch", msg.clone());
                    self.set_link(Link::Failed(msg));
                    self.retry_after = Some(Instant::now() + MISMATCH_RETRY);
                    return;
                }
            }
        }
        let Ok(sock) = self.p.sock.try_clone() else { return };
        *self.shared.lock(&self.shared.server_fingerprint) = fp;
        self.ephemeral = None;
        self.last_heard = Instant::now();
        *self.slot.get() = Some(Channel { sock, peer: from, session, buf: Vec::new(), plain: Vec::new() });
        self.last_keepalive = Instant::now() - KEEPALIVE_EVERY;
        *self.shared.lock(&self.shared.via) = format!("{} ({from})", self.p.via);
        log_info(format_args!("net: handshake with {from} ({})", self.p.via));
        if !self.joined() {
            self.set_link(Link::Connecting);
        }
    }

    fn rejected(&mut self, reason: String) {
        if self.last_reject.as_ref() != Some(&reason) {
            let code = if reason.contains("protocol") {
                "version_mismatch"
            } else {
                "rejected"
            };
            events::report(Level::Error, self.p.phase, code, tr!("il server ha rifiutato: {reason}", "the server refused: {reason}"));
            self.last_reject = Some(reason.clone());
        }
        self.set_link(Link::Failed(reason));
    }

    fn message(&mut self, msg: ServerMsg) {
        let shared = self.shared.clone();
        match msg {
            ServerMsg::Welcome { id, range } => {
                let mut link = shared.lock(&shared.link);
                if link.joined().is_none() {
                    log_info(format_args!("net: joined as #{id} (range {range} m, {})", self.p.via));
                    events::report(Level::Info, self.p.phase, "connected", tr!("collegato alla chat vocale ({})", "connected to the voice chat ({})", self.p.via));
                    for code in ["server_lost", "rejected", "server_unreachable"] {
                        events::resolve(code);
                    }
                    self.sent_party = None;
                    self.last_reject = None;
                }
                *link = Link::Joined { id, range };
            }
            ServerMsg::Reject { reason } => {
                if reason == "not joined" {
                    self.set_link(Link::Connecting);
                    self.sent_party = None;
                } else {
                    self.rejected(reason);
                }
            }
            ServerMsg::Voice { from, party, seq, pos, env, opus } => {
                let Some(opus) = self.p.voice_key.open(from, seq, &opus) else {
                    self.decrypt_failures += 1;
                    if self.decrypt_failures == 25 {
                        events::report(Level::Warn, self.p.phase, "decrypt_failed", tr!("voce non decifrabile dal giocatore #{from}", "undecryptable voice from player #{from}"));
                    }
                    return;
                };
                let now = Instant::now();
                shared
                    .lock(&shared.speakers)
                    .entry(from)
                    .and_modify(|s| {
                        s.pos = pos;
                        s.party = party;
                        s.env = env;
                        s.last_packet = now;
                    })
                    .or_insert(SpeakerInfo { pos, party, env, last_packet: now, level_db: -100.0, audible_until: None });
                shared.lock(&shared.inbox).push((from, seq, opus, false));
            }
            ServerMsg::Roster { players } => {
                let list: Vec<String> = players
                    .iter()
                    .map(|p| match p.party.as_str() {
                        "" => format!("{:?} #{}", p.name, p.id),
                        party => format!("{:?} #{} [{party:?}]", p.name, p.id),
                    })
                    .collect();
                log_info(format_args!("net: room: {}", list.join(", ")));
                shared.lock(&shared.speakers).retain(|id, _| players.iter().any(|p| p.id == *id));
                *shared.lock(&shared.roster) = players;
            }
        }
    }
}
