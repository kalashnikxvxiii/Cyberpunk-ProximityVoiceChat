//! Connection to the voice server and the per-speaker jitter buffers.

use std::collections::{BTreeMap, HashMap};
use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vc_protocol::{ClientMsg, MAX_PACKET, PlayerId, ServerMsg, VERSION, Vec3};

use crate::config::{self, Config};
use crate::game::Pose;
use crate::log_info;

pub const FRAME: usize = 960;
const HELLO_EVERY: Duration = Duration::from_secs(1);
const POSE_EVERY: Duration = Duration::from_millis(200);
const NO_ANSWER_WARN: Duration = Duration::from_secs(5);
const CONFIG_CHECK_EVERY: Duration = Duration::from_secs(2);
/// Frames buffered before a speaker starts playing (40 ms).
const START_DEPTH: usize = 2;
/// Above this the buffer is trimmed, to keep latency bounded.
const MAX_DEPTH: usize = 8;
/// Lost frames concealed before a speaker is considered silent.
const MAX_MISSING: u32 = 5;

/// Settings that can change while the game runs (config.ini is re-read).
#[derive(Clone, Copy)]
pub struct Live {
    pub ptt_vk: i32,
    pub volume: f32,
    pub mic_gain: f32,
}

impl Live {
    fn from(cfg: &Config) -> Self {
        Self { ptt_vk: cfg.ptt_vk, volume: cfg.volume, mic_gain: cfg.mic_gain }
    }
}

/// State shared between the game tick, the network thread and the audio thread.
pub struct Shared {
    /// As loaded at start; server, room and name only apply on restart.
    pub cfg: Config,
    pub live: Mutex<Live>,
    pub pose: Mutex<Option<Pose>>,
    pub link: Mutex<Option<Link>>,
    pub speakers: Mutex<HashMap<PlayerId, Speaker>>,
    pub party: Mutex<Option<String>>,
    pub stop: AtomicBool,
}

#[derive(Clone, Copy)]
pub struct Link {
    pub range: f32,
}

impl Shared {
    pub fn new(cfg: Config) -> Self {
        let party = (!cfg.party.is_empty()).then(|| cfg.party.clone());
        Self {
            live: Mutex::new(Live::from(&cfg)),
            cfg,
            pose: Mutex::new(None),
            link: Mutex::new(None),
            speakers: Mutex::new(HashMap::new()),
            party: Mutex::new(party),
            stop: AtomicBool::new(false),
        }
    }
}

pub struct Speaker {
    pub pos: Vec3,
    pub party: bool,
    queue: BTreeMap<u32, Vec<u8>>,
    next: Option<u32>,
    missing: u32,
    decoder: opus::Decoder,
}

impl Speaker {
    fn new() -> Option<Self> {
        Some(Self {
            pos: [0.0; 3],
            party: false,
            queue: BTreeMap::new(),
            next: None,
            missing: 0,
            decoder: opus::Decoder::new(48_000, opus::Channels::Mono).ok()?,
        })
    }

    fn push(&mut self, seq: u32, opus: Vec<u8>) {
        if self.next.is_some_and(|n| seq < n) {
            return;
        }
        self.queue.insert(seq, opus);
        while self.queue.len() > MAX_DEPTH {
            self.queue.pop_first();
            self.next = self.queue.first_key_value().map(|(k, _)| *k);
        }
    }

    /// Decodes the next 20 ms into `out`. False when the speaker is silent.
    pub fn next_frame(&mut self, out: &mut [f32; FRAME]) -> bool {
        let next = match self.next {
            Some(n) => n,
            None if self.queue.len() >= START_DEPTH => *self.queue.first_key_value().unwrap().0,
            None => return false,
        };
        let decoded = match self.queue.remove(&next) {
            Some(pkt) => {
                self.missing = 0;
                self.decoder.decode_float(&pkt, out, false)
            }
            None => {
                self.missing += 1;
                if self.missing > MAX_MISSING {
                    self.next = None;
                    self.missing = 0;
                    return false;
                }
                // Packet loss concealment.
                self.decoder.decode_float(&[], out, false)
            }
        };
        self.next = Some(next.wrapping_add(1));
        decoded.is_ok_and(|n| n == FRAME)
    }
}

pub struct Net {
    sock: UdpSocket,
}

impl Net {
    pub fn connect(server: &str) -> Result<Self, String> {
        let addr = server
            .to_socket_addrs()
            .map_err(|e| format!("server '{server}': {e}"))?
            .next()
            .ok_or(format!("server '{server}': no address"))?;
        let sock = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| e.to_string())?;
        sock.connect(addr).map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
        log_info(format_args!("net: server {addr}"));
        Ok(Self { sock })
    }

    pub fn sender(&self) -> Option<Sender> {
        self.sock.try_clone().ok().map(|sock| Sender { sock, buf: Vec::new() })
    }

    /// Network loop; runs until `shared.stop`.
    pub fn run(self, shared: Arc<Shared>) {
        let Some(mut tx) = self.sender() else { return };
        let mut packet = [0u8; MAX_PACKET + 1];
        let mut last_hello: Option<Instant> = None;
        let mut last_pose = Instant::now();
        let mut sent_party: Option<String> = None;
        let mut last_reject: Option<String> = None;
        let started = Instant::now();
        let mut warned_silent = false;
        let mut config_mtime = config::modified();
        let mut last_config_check = Instant::now();
        let cfg = &shared.cfg;

        while !shared.stop.load(Ordering::Relaxed) {
            let joined = shared.link.lock().unwrap().is_some();
            let pose = *shared.pose.lock().unwrap();

            // Hello until welcomed; afterwards it doubles as keepalive while
            // there is no pose to send (menus, loading screens).
            let hello_due = last_hello.is_none_or(|t| t.elapsed() >= HELLO_EVERY);
            if (!joined || pose.is_none()) && hello_due {
                last_hello = Some(Instant::now());
                tx.send(&ClientMsg::Hello {
                    version: VERSION,
                    room: cfg.room.clone(),
                    name: cfg.name.clone(),
                });
            }
            if joined {
                if let Some(p) = pose
                    && last_pose.elapsed() >= POSE_EVERY
                {
                    last_pose = Instant::now();
                    tx.send(&ClientMsg::Pose { pos: p.pos() });
                }
                let want = shared.party.lock().unwrap().clone();
                if want != sent_party {
                    tx.send(&ClientMsg::Party { name: want.clone().unwrap_or_default() });
                    sent_party = want;
                }
            }

            if last_config_check.elapsed() >= CONFIG_CHECK_EVERY {
                last_config_check = Instant::now();
                let mtime = config::modified();
                if mtime != config_mtime {
                    config_mtime = mtime;
                    reload_config(&shared);
                }
            }

            if !joined && !warned_silent && started.elapsed() >= NO_ANSWER_WARN {
                warned_silent = true;
                log_info(format_args!(
                    "net: no answer from {} yet (server down or UDP blocked); still trying",
                    cfg.server
                ));
            }

            let n = match self.sock.recv(&mut packet) {
                Ok(n) if n <= MAX_PACKET => n,
                _ => continue,
            };
            match ServerMsg::decode(&packet[..n]) {
                Some(ServerMsg::Welcome { id, range }) => {
                    let mut link = shared.link.lock().unwrap();
                    if link.is_none() {
                        log_info(format_args!(
                            "net: joined room '{}' as '{}' (#{id}, range {range} m)",
                            cfg.room, cfg.name
                        ));
                        sent_party = None;
                    }
                    *link = Some(Link { range });
                    last_reject = None;
                }
                Some(ServerMsg::Reject { reason }) => {
                    // Hello is retried by its timer; log each distinct reason once.
                    shared.link.lock().unwrap().take();
                    if last_reject.as_ref() != Some(&reason) {
                        log_info(format_args!("net: rejected: {reason}"));
                        last_reject = Some(reason);
                    }
                }
                Some(ServerMsg::Voice { from, party, seq, pos, opus }) => {
                    let mut speakers = shared.speakers.lock().unwrap();
                    let s = match speakers.get_mut(&from) {
                        Some(s) => s,
                        None => match Speaker::new() {
                            Some(s) => speakers.entry(from).or_insert(s),
                            None => continue,
                        },
                    };
                    s.pos = pos;
                    s.party = party;
                    s.push(seq, opus);
                }
                Some(ServerMsg::Roster { players }) => {
                    let list: Vec<String> = players
                        .iter()
                        .map(|p| match p.party.as_str() {
                            "" => format!("{} #{}", p.name, p.id),
                            party => format!("{} #{} [{party}]", p.name, p.id),
                        })
                        .collect();
                    log_info(format_args!("net: room: {}", list.join(", ")));
                    shared.speakers.lock().unwrap().retain(|id, _| {
                        *id == vc_protocol::ECHO_ID || players.iter().any(|p| p.id == *id)
                    });
                }
                None => {}
            }
        }
        tx.send(&ClientMsg::Bye);
    }
}

/// Applies the settings that can change live; the rest needs a restart.
fn reload_config(shared: &Shared) {
    let new = config::load();
    let old = &shared.cfg;
    *shared.live.lock().unwrap() = Live::from(&new);
    *shared.party.lock().unwrap() = (!new.party.is_empty()).then(|| new.party.clone());
    log_info(format_args!(
        "config: reloaded: party '{}' ptt 0x{:02X} volume {} mic_gain {}",
        new.party, new.ptt_vk, new.volume, new.mic_gain
    ));
    if (&new.server, &new.room, &new.name) != (&old.server, &old.room, &old.name) {
        log_info(format_args!("config: server, room and name apply after restarting the game"));
    }
}

pub struct Sender {
    sock: UdpSocket,
    buf: Vec<u8>,
}

impl Sender {
    pub fn send(&mut self, msg: &ClientMsg) {
        msg.encode(&mut self.buf);
        let _ = self.sock.send(&self.buf);
    }
}
