//! An in-memory NightCoop with several machines, for tests: a session with a
//! host (peer 0) and guests, passports (same id and protocol), the host relaying
//! guest-to-guest messages, shared state kept by the host, the rate limits, and
//! a network with latency, jitter, losses, and frames that stall (loading
//! screens).
//!
//! The limits count as NightCoop's developer described them (2026-11-06): one
//! `send` is one message, `TO_ALL` included; its bytes count once if every
//! partner has the mod, per recipient otherwise; the host's relay between guests
//! is not charged to the host's mod; incoming limits are per sender. Delivery
//! and frames go on out of the world (menus, loading); a loading screen is a
//! stall, and what arrived meanwhile comes in one frame. The budget is a token
//! bucket holding one second of it, so a short burst reaches 4x the rate.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::ffi;
use crate::{Coop, E_ARG, E_DENIED, E_LIMIT, E_STATE, E_UNKNOWN, Hooks, PartnerState, Peer, TO_ALL, Where};

#[derive(Debug, Clone)]
pub struct Net {
    /// One way, per hop (a guest's message to another guest makes two).
    pub latency_ms: f64,
    /// Extra delay, uniform in 0..jitter_ms.
    pub jitter_ms: f64,
    /// Loss of an unreliable message, per hop.
    pub loss: f64,
    /// Until two machines know each other's passport.
    pub passport_ms: f64,
    pub seed: u64,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            latency_ms: 30.0,
            jitter_ms: 10.0,
            loss: 0.0,
            passport_ms: 200.0,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// Per machine, for the assertions.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub sent: u64,
    pub sent_bytes: u64,
    /// `send`/`state_set` answered `E_LIMIT`.
    pub limited: u64,
    /// Incoming over the per-sender limit.
    pub dropped_in: u64,
    pub lost: u64,
    pub received: u64,
}

#[derive(Debug, Clone)]
enum Delivery {
    Event(i32, Peer),
    Message { from: Peer, channel: u16, data: Vec<u8> },
    State { key: String, data: Option<Vec<u8>>, by: Peer },
    Request { from: Peer, key: String, data: Vec<u8> },
}

struct Packet {
    at: f64,
    to: usize,
    from: usize,
    bytes: usize,
    d: Delivery,
}

/// Token bucket over messages and bytes.
#[derive(Debug, Clone)]
struct Bucket {
    msgs: f64,
    bytes: f64,
    at: f64,
}

impl Bucket {
    fn new() -> Self {
        Self { msgs: f64::from(crate::RATE_MESSAGES_PER_S), bytes: f64::from(crate::RATE_BYTES_PER_S), at: 0.0 }
    }

    fn take(&mut self, now: f64, msgs: usize, bytes: usize) -> bool {
        let (rm, rb) = (f64::from(crate::RATE_MESSAGES_PER_S), f64::from(crate::RATE_BYTES_PER_S));
        let dt = (now - self.at).max(0.0) / 1000.0;
        self.at = now;
        self.msgs = (self.msgs + rm * dt).min(rm);
        self.bytes = (self.bytes + rb * dt).min(rb);
        if self.msgs < msgs as f64 || self.bytes < bytes as f64 {
            return false;
        }
        self.msgs -= msgs as f64;
        self.bytes -= bytes as f64;
        true
    }
}

struct Machine {
    protocol: Option<u32>,
    name: String,
    joined: Option<f64>,
    in_world: bool,
    frame_ms: f64,
    frame_jitter_ms: f64,
    next_frame: f64,
    last_frame: f64,
    stalled_until: f64,
    shut: bool,
    state: BTreeMap<String, Vec<u8>>,
    queue: VecDeque<Delivery>,
    out: Bucket,
    incoming: HashMap<usize, Bucket>,
    stats: Stats,
    log: Vec<String>,
}

struct World {
    now: f64,
    net: Net,
    rng: u64,
    /// Mods are off for the session (the host's `sdk_session` switch).
    session_denied: bool,
    m: Vec<Machine>,
    flight: Vec<Packet>,
    /// Reliable streams arrive in order: last arrival per (from, to, channel).
    order: HashMap<(usize, usize, u16), f64>,
    known: HashSet<(usize, usize)>,
    together: HashSet<(usize, usize)>,
}

impl World {
    fn rand(&mut self) -> f64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    fn in_session(&self, i: usize) -> bool {
        self.m[i].joined.is_some() && self.m[0].joined.is_some()
    }

    fn passport_known(&self, a: usize, b: usize) -> bool {
        self.known.contains(&(a.min(b), a.max(b)))
    }

    fn compatible(&self, a: usize, b: usize) -> bool {
        self.m[a].protocol.is_some() && self.m[a].protocol == self.m[b].protocol
    }

    fn hop(&mut self) -> f64 {
        self.net.latency_ms + self.rand() * self.net.jitter_ms
    }

    /// Schedules `d` from `from` to `to`; false if it was lost on the way.
    fn route(&mut self, from: usize, to: usize, reliable: bool, channel: Option<u16>, bytes: usize, d: Delivery) {
        let hops = if from != 0 && to != 0 { 2 } else { 1 };
        let mut at = self.now;
        for _ in 0..hops {
            if !reliable && self.rand() < self.net.loss {
                self.m[from].stats.lost += 1;
                return;
            }
            at += self.hop();
        }
        if reliable && let Some(ch) = channel {
            let last = self.order.entry((from, to, ch)).or_insert(at);
            at = at.max(*last);
            *last = at;
        }
        self.flight.push(Packet { at, to, from, bytes, d });
    }

    fn local(&mut self, i: usize, d: Delivery) {
        if self.m[i].protocol.is_some() {
            self.m[i].queue.push_back(d);
        }
    }

    fn peers_of(&self, i: usize) -> Vec<usize> {
        (0..self.m.len()).filter(|&j| j != i && self.in_session(i) && self.in_session(j)).collect()
    }

    /// The bytes one message to `targets` costs `from`: once if every partner has the mod.
    fn cost(&self, from: usize, targets: &[usize], len: usize) -> usize {
        if targets.len() == self.peers_of(from).len() { len } else { len * targets.len() }
    }

    fn send(&mut self, from: usize, to: Peer, channel: u16, data: &[u8], flags: u32) -> i32 {
        let reliable = flags & crate::SEND_RELIABLE != 0;
        let max = if reliable { crate::RELIABLE_MAX } else { crate::UNRELIABLE_MAX } as usize;
        if data.is_empty() || data.len() > max {
            return E_ARG;
        }
        if !self.in_session(from) {
            return E_STATE;
        }
        if self.session_denied {
            return E_DENIED;
        }
        let targets: Vec<usize> = if to == TO_ALL {
            self.peers_of(from).into_iter().filter(|&j| self.passport_known(from, j) && self.compatible(from, j)).collect()
        } else {
            let j = to as usize;
            if j >= self.m.len() || j == from || !self.in_session(j) {
                return E_UNKNOWN;
            }
            if !self.passport_known(from, j) {
                return E_STATE;
            }
            if !self.compatible(from, j) { vec![] } else { vec![j] }
        };
        if targets.is_empty() {
            return 0;
        }
        let bytes = if to == TO_ALL { self.cost(from, &targets, data.len()) } else { data.len() };
        let now = self.now;
        if !self.m[from].out.take(now, 1, bytes) {
            self.m[from].stats.limited += 1;
            return E_LIMIT;
        }
        self.m[from].stats.sent += 1;
        self.m[from].stats.sent_bytes += bytes as u64;
        for &j in &targets {
            let d = Delivery::Message { from: from as Peer, channel, data: data.to_vec() };
            self.route(from, j, reliable, Some(channel), data.len(), d);
        }
        targets.len() as i32
    }

    fn state_set(&mut self, from: usize, key: &str, data: &[u8]) -> i32 {
        if key.is_empty() || key.len() > crate::STATE_KEY_MAX as usize || data.len() > crate::STATE_VALUE_MAX as usize {
            return E_ARG;
        }
        if !self.in_session(from) || from == 0 {
            if self.in_session(from) && self.session_denied {
                return E_DENIED;
            }
            return self.apply_state(from, key, data, from as Peer);
        }
        if !self.passport_known(from, 0) {
            return E_STATE;
        }
        if self.session_denied || !self.compatible(from, 0) {
            return E_DENIED;
        }
        let now = self.now;
        if !self.m[from].out.take(now, 1, key.len() + data.len()) {
            self.m[from].stats.limited += 1;
            return E_LIMIT;
        }
        let d = Delivery::Request { from: from as Peer, key: key.into(), data: data.to_vec() };
        self.route(from, 0, true, None, key.len() + data.len(), d);
        crate::OK
    }

    /// On the host (or alone): store, tell ourselves and every peer with the mod.
    fn apply_state(&mut self, at: usize, key: &str, data: &[u8], by: Peer) -> i32 {
        let value = (!data.is_empty()).then(|| data.to_vec());
        match &value {
            Some(v) => self.m[at].state.insert(key.into(), v.clone()),
            None => self.m[at].state.remove(key),
        };
        self.local(at, Delivery::State { key: key.into(), data: value.clone(), by });
        let targets: Vec<usize> =
            self.peers_of(at).into_iter().filter(|&j| self.passport_known(at, j) && self.compatible(at, j)).collect();
        if !targets.is_empty() {
            let now = self.now;
            let bytes = self.cost(at, &targets, key.len() + data.len());
            if !self.m[at].out.take(now, 1, bytes) {
                // The host's own copy changed; NightCoop would retry the spread. Counted, not modelled.
                self.m[at].stats.limited += 1;
            }
        }
        for j in targets {
            let d = Delivery::State { key: key.into(), data: value.clone(), by };
            self.route(at, j, true, None, key.len() + data.len(), d);
        }
        crate::OK
    }

    /// Passports and world presence, as NightCoop's events.
    fn tick_session(&mut self) {
        let n = self.m.len();
        for a in 0..n {
            for b in a + 1..n {
                let both = self.in_session(a) && self.in_session(b);
                let pair = (a, b);
                let since = self.m[a].joined.unwrap_or(f64::MAX).max(self.m[b].joined.unwrap_or(f64::MAX));
                if both && !self.known.contains(&pair) && self.now >= since + self.net.passport_ms {
                    self.known.insert(pair);
                    for (x, y) in [(a, b), (b, a)] {
                        let e = if self.compatible(x, y) { crate::ev::PEER_MOD } else { crate::ev::PEER_NO_MOD };
                        self.local(x, Delivery::Event(e, y as Peer));
                    }
                    // A late joiner gets the whole state from the host.
                    if a == 0 && self.compatible(0, b) {
                        let keys: Vec<(String, Vec<u8>)> =
                            self.m[0].state.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in keys {
                            let len = k.len() + v.len();
                            self.route(0, b, true, None, len, Delivery::State { key: k, data: Some(v), by: 0 });
                        }
                    }
                }
                let together = both && self.m[a].in_world && self.m[b].in_world;
                if together != self.together.contains(&pair) {
                    let e = if together { crate::ev::PEER_WORLD_IN } else { crate::ev::PEER_WORLD_OUT };
                    if together { self.together.insert(pair) } else { self.together.remove(&pair) };
                    self.local(a, Delivery::Event(e, b as Peer));
                    self.local(b, Delivery::Event(e, a as Peer));
                }
            }
        }
        let now = self.now;
        let mut arrived = Vec::new();
        self.flight.retain_mut(|p| {
            if p.at <= now {
                arrived.push(Packet { at: p.at, to: p.to, from: p.from, bytes: p.bytes, d: p.d.clone() });
                false
            } else {
                true
            }
        });
        arrived.sort_by(|x, y| x.at.total_cmp(&y.at));
        for p in arrived {
            // Gone from the session meanwhile: dropped.
            if !self.in_session(p.to) || !self.in_session(p.from) {
                continue;
            }
            let ok = self.m[p.to].incoming.entry(p.from).or_insert_with(Bucket::new).take(now, 1, p.bytes);
            if !ok {
                self.m[p.to].stats.dropped_in += 1;
                continue;
            }
            self.m[p.to].stats.received += 1;
            if let Delivery::State { key, data, .. } = &p.d {
                match data {
                    Some(v) => self.m[p.to].state.insert(key.clone(), v.clone()),
                    None => self.m[p.to].state.remove(key),
                };
            }
            self.local(p.to, p.d);
        }
    }

    fn clear_state(&mut self, i: usize) {
        for k in std::mem::take(&mut self.m[i].state).into_keys() {
            self.local(i, Delivery::State { key: k, data: None, by: 0 });
        }
    }
}

/// One machine's view, handed to its hooks.
struct View<'a> {
    w: &'a RefCell<World>,
    me: usize,
}

impl Coop for View<'_> {
    fn where_(&self) -> Where {
        let w = self.w.borrow();
        let in_session = w.in_session(self.me);
        Where {
            in_session,
            is_host: in_session && self.me == 0,
            in_world: w.m[self.me].in_world,
            in_host_world: in_session && w.m[self.me].in_world,
            local_peer: if in_session { self.me as Peer } else { crate::NONE },
            peer_count: w.peers_of(self.me).len() as u32,
            shared_time_ms: if in_session { w.now as u64 } else { 0 },
        }
    }

    fn peers(&self) -> Vec<Peer> {
        self.w.borrow().peers_of(self.me).into_iter().map(|j| j as Peer).collect()
    }

    fn peer_name(&self, peer: Peer) -> String {
        let name = self.w.borrow().m.get(peer as usize).map(|m| m.name.clone()).unwrap_or_default();
        self.clean_text(name.as_bytes(), 0)
    }

    fn peer_has_mod(&self, peer: Peer) -> i32 {
        let w = self.w.borrow();
        let j = peer as usize;
        if j >= w.m.len() || !w.passport_known(self.me, j) {
            return E_STATE;
        }
        w.compatible(self.me, j) as i32
    }

    fn send(&self, to: Peer, channel: u16, data: &[u8], flags: u32) -> i32 {
        self.w.borrow_mut().send(self.me, to, channel, data, flags)
    }

    fn state_set(&self, key: &str, data: &[u8]) -> i32 {
        self.w.borrow_mut().state_set(self.me, key, data)
    }

    fn state_get(&self, key: &str) -> Option<Vec<u8>> {
        self.w.borrow().m[self.me].state.get(key).cloned()
    }

    fn partner_body(&self, peer: Peer) -> u64 {
        let w = self.w.borrow();
        let j = peer as usize;
        let pair = (self.me.min(j), self.me.max(j));
        if w.together.contains(&pair) { 0xB0D1_0000 + u64::from(peer) } else { 0 }
    }

    fn partner_state(&self, peer: Peer) -> Option<PartnerState> {
        let w = self.w.borrow();
        let j = peer as usize;
        if j >= w.m.len() || !w.in_session(self.me) || !w.in_session(j) {
            return None;
        }
        let pair = (self.me.min(j), self.me.max(j));
        let (flags, place) = if j == self.me {
            (ffi::NCSDK_PARTNER_SELF, ffi::NCSDK_WHERE_HOST_WORLD)
        } else if w.together.contains(&pair) {
            (ffi::NCSDK_PARTNER_IN_WORLD, ffi::NCSDK_WHERE_HOST_WORLD)
        } else {
            (0, ffi::NCSDK_WHERE_LOADING)
        };
        Some(PartnerState { flags, place: place as i32 })
    }

    fn clean_text(&self, text: &[u8], max: usize) -> String {
        clean(text, max)
    }

    fn log(&self, level: i32, text: &str) {
        let line = format!("{level} {}", clean(text.as_bytes(), 1024));
        self.w.borrow_mut().m[self.me].log.push(line);
    }
}

/// What `cleanText` promises, roughly: valid UTF-8, no control, bidi or zero-width characters, bounded.
pub fn clean(text: &[u8], max: usize) -> String {
    let max = if max == 0 { ffi::NCSDK_TEXT_MAX as usize } else { max };
    let text = String::from_utf8_lossy(&text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())]);
    let mut out = String::new();
    for c in text.chars() {
        let c = if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c };
        let invisible = matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
        if c.is_control() || invisible || (c == ' ' && out.ends_with(' ')) {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The fake NightCoop: machines 0..n, the host is machine 0. Every machine runs hooks of type `H`
/// (or none: a player without the mod).
pub struct Fake<H: Hooks> {
    w: RefCell<World>,
    hooks: Vec<Option<H>>,
}

impl<H: Hooks> Fake<H> {
    pub fn new(net: Net) -> Self {
        let rng = net.seed.max(1);
        Self {
            w: RefCell::new(World {
                now: 0.0,
                net,
                rng,
                session_denied: false,
                m: Vec::new(),
                flight: Vec::new(),
                order: HashMap::new(),
                known: HashSet::new(),
                together: HashSet::new(),
            }),
            hooks: Vec::new(),
        }
    }

    /// A machine running our mod with this `protocol` (`None` with `hooks` `None`: without the mod).
    /// Returns its peer number. Starts out of session, in the world, at 60 fps.
    pub fn add(&mut self, protocol: Option<u32>, hooks: Option<H>) -> Peer {
        let mut w = self.w.borrow_mut();
        let i = w.m.len();
        w.m.push(Machine {
            protocol,
            name: format!("Player{i}"),
            joined: None,
            in_world: true,
            frame_ms: 1000.0 / 60.0,
            frame_jitter_ms: 0.0,
            next_frame: 0.0,
            last_frame: 0.0,
            stalled_until: 0.0,
            shut: false,
            state: BTreeMap::new(),
            queue: VecDeque::new(),
            out: Bucket::new(),
            incoming: HashMap::new(),
            stats: Stats::default(),
            log: Vec::new(),
        });
        self.hooks.push(hooks);
        i as Peer
    }

    pub fn now_ms(&self) -> f64 {
        self.w.borrow().now
    }

    pub fn set_name(&mut self, peer: Peer, name: &str) {
        self.w.borrow_mut().m[peer as usize].name = name.into();
    }

    pub fn set_fps(&mut self, peer: Peer, fps: f64, jitter_ms: f64) {
        let mut w = self.w.borrow_mut();
        w.m[peer as usize].frame_ms = 1000.0 / fps;
        w.m[peer as usize].frame_jitter_ms = jitter_ms;
    }

    pub fn set_net(&mut self, f: impl FnOnce(&mut Net)) {
        f(&mut self.w.borrow_mut().net);
    }

    /// The host's `sdk_session` switch off: no mod data in the session.
    pub fn deny_session(&mut self, denied: bool) {
        self.w.borrow_mut().session_denied = denied;
    }

    /// Machine 0 hosts a session.
    pub fn host(&mut self) {
        let mut w = self.w.borrow_mut();
        let now = w.now;
        w.m[0].joined = Some(now);
        w.local(0, Delivery::Event(crate::ev::SESSION_START, 0));
    }

    pub fn join(&mut self, peer: Peer) {
        let mut w = self.w.borrow_mut();
        let i = peer as usize;
        let now = w.now;
        w.clear_state(i);
        w.m[i].joined = Some(now);
        w.local(i, Delivery::Event(crate::ev::SESSION_START, peer));
        for j in w.peers_of(i) {
            w.local(i, Delivery::Event(crate::ev::PEER_JOIN, j as Peer));
            w.local(j, Delivery::Event(crate::ev::PEER_JOIN, peer));
        }
    }

    /// A guest leaves; the host leaving ends the session for everybody.
    pub fn leave(&mut self, peer: Peer) {
        let leaving: Vec<usize> = {
            let w = self.w.borrow();
            if peer == 0 { (0..w.m.len()).filter(|&i| w.m[i].joined.is_some()).collect() } else { vec![peer as usize] }
        };
        let mut w = self.w.borrow_mut();
        for &i in &leaving {
            for j in w.peers_of(i) {
                if !leaving.contains(&j) {
                    let pair = (i.min(j), i.max(j));
                    if w.together.remove(&pair) {
                        w.local(j, Delivery::Event(crate::ev::PEER_WORLD_OUT, i as Peer));
                    }
                    w.local(j, Delivery::Event(crate::ev::PEER_LEAVE, i as Peer));
                }
            }
        }
        for &i in &leaving {
            w.known.retain(|&(a, b)| a != i && b != i);
            w.together.retain(|&(a, b)| a != i && b != i);
            w.order.retain(|&(a, b, _), _| a != i && b != i);
            w.m[i].joined = None;
            w.clear_state(i);
            w.local(i, Delivery::Event(crate::ev::SESSION_END, crate::NONE));
        }
    }

    /// In the world, or in a menu / loading.
    pub fn set_world(&mut self, peer: Peer, in_world: bool) {
        self.w.borrow_mut().m[peer as usize].in_world = in_world;
    }

    /// No frame (and so no hooks) for this long: a loading screen.
    pub fn stall(&mut self, peer: Peer, ms: f64) {
        let mut w = self.w.borrow_mut();
        let until = w.now + ms;
        w.m[peer as usize].stalled_until = until;
    }

    /// NightCoop unloads on this machine.
    pub fn shutdown(&mut self, peer: Peer) {
        let i = peer as usize;
        self.w.borrow_mut().m[i].shut = true;
        if let Some(h) = self.hooks[i].as_mut() {
            h.shutdown();
        }
    }

    /// A partner's (possibly broken or malicious) copy of the mod sends raw bytes: delivered at the next frame.
    pub fn inject(&mut self, to: Peer, from: Peer, channel: u16, data: &[u8]) {
        self.w.borrow_mut().local(to as usize, Delivery::Message { from, channel, data: data.to_vec() });
    }

    /// A guest asks the host to set a key, bypassing its own mod: what the host's hooks decide.
    pub fn inject_state_request(&mut self, from: Peer, key: &str, data: &[u8]) -> bool {
        let view = View { w: &self.w, me: 0 };
        let Some(h) = self.hooks[0].as_mut() else { return false };
        if !h.state_request(&view, from, key, data) {
            return false;
        }
        self.w.borrow_mut().apply_state(0, key, data, from);
        true
    }

    pub fn hooks(&mut self, peer: Peer) -> &mut H {
        self.hooks[peer as usize].as_mut().expect("a machine with the mod")
    }

    pub fn stats(&self, peer: Peer) -> Stats {
        self.w.borrow().m[peer as usize].stats.clone()
    }

    pub fn log(&self, peer: Peer) -> Vec<String> {
        self.w.borrow().m[peer as usize].log.clone()
    }

    pub fn state(&self, peer: Peer, key: &str) -> Option<Vec<u8>> {
        self.w.borrow().m[peer as usize].state.get(key).cloned()
    }

    /// Runs every machine's frames for `ms` of simulated time.
    pub fn run(&mut self, ms: f64) {
        let end = self.w.borrow().now + ms;
        loop {
            let next = {
                let w = self.w.borrow();
                (0..w.m.len()).min_by(|&a, &b| w.m[a].next_frame.total_cmp(&w.m[b].next_frame))
            };
            let Some(i) = next else { break };
            let at = self.w.borrow().m[i].next_frame;
            if at > end {
                break;
            }
            self.frame_of(i, at);
        }
        let mut w = self.w.borrow_mut();
        w.now = end;
        w.tick_session();
    }

    fn frame_of(&mut self, i: usize, at: f64) {
        let (deliveries, dt) = {
            let mut w = self.w.borrow_mut();
            w.now = w.now.max(at);
            w.tick_session();
            let jitter = w.m[i].frame_jitter_ms * w.rand();
            let m = &mut w.m[i];
            m.next_frame = at + m.frame_ms + jitter;
            if at < m.stalled_until || m.shut {
                return;
            }
            let dt = ((at - m.last_frame) / 1000.0) as f32;
            m.last_frame = at;
            (std::mem::take(&mut m.queue), dt)
        };
        let view = View { w: &self.w, me: i };
        let Some(h) = self.hooks[i].as_mut() else { return };
        for d in deliveries {
            match d {
                Delivery::Event(e, p) => h.event(&view, e, p),
                Delivery::Message { from, channel, data } => h.message(&view, from, channel, &data),
                Delivery::State { key, data, by } => h.state_changed(&view, &key, data.as_deref(), by),
                Delivery::Request { from, key, data } => {
                    if h.state_request(&view, from, &key, &data) {
                        self.w.borrow_mut().apply_state(0, &key, &data, from);
                    } else {
                        let line = format!("refused state request from {from}: {key}");
                        self.w.borrow_mut().m[0].log.push(line);
                    }
                }
            }
        }
        h.frame(&view, dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records everything and echoes channel 1 back to its sender.
    #[derive(Default)]
    struct Probe {
        seen: Vec<String>,
        frames: u32,
    }

    impl Hooks for Probe {
        fn frame(&mut self, _: &dyn Coop, _: f32) {
            self.frames += 1;
        }
        fn event(&mut self, _: &dyn Coop, e: i32, p: Peer) {
            self.seen.push(format!("ev{e}:{p}"));
        }
        fn message(&mut self, c: &dyn Coop, from: Peer, ch: u16, d: &[u8]) {
            self.seen.push(format!("msg{ch}:{from}:{}", String::from_utf8_lossy(d)));
            if ch == 1 {
                c.send(from, 2, d, 0);
            }
        }
        fn state_changed(&mut self, _: &dyn Coop, k: &str, d: Option<&[u8]>, by: Peer) {
            self.seen.push(format!("state:{k}={:?}:{by}", d.map(|d| String::from_utf8_lossy(d).into_owned())));
        }
        fn state_request(&mut self, _: &dyn Coop, _: Peer, k: &str, _: &[u8]) -> bool {
            k != "sys"
        }
        fn shutdown(&mut self) {
            self.seen.push("shutdown".into());
        }
    }

    fn session(guests: usize) -> Fake<Probe> {
        let mut f = Fake::new(Net { jitter_ms: 0.0, ..Net::default() });
        for _ in 0..=guests {
            f.add(Some(1), Some(Probe::default()));
        }
        f.host();
        for g in 1..=guests {
            f.join(g as Peer);
        }
        f.run(300.0);
        f
    }

    fn view(f: &Fake<Probe>, me: usize) -> View<'_> {
        View { w: &f.w, me }
    }

    #[test]
    fn passports_then_messages_relayed_between_guests() {
        let mut f = session(2);
        assert!(f.hooks(1).seen.contains(&"ev7:2".to_string()), "{:?}", f.hooks(1).seen);
        assert_eq!(view(&f, 1).send(2, 1, b"hi", 0), 1);
        f.run(200.0);
        assert!(f.hooks(2).seen.contains(&"msg1:1:hi".to_string()));
        assert!(f.hooks(1).seen.contains(&"msg2:2:hi".to_string()));
        assert_eq!(view(&f, 0).send(TO_ALL, 3, b"x", 0), 2);
        assert_eq!(view(&f, 0).send(TO_ALL, 3, &[0; 1025], 0), E_ARG);
    }

    #[test]
    fn before_the_passport_send_is_not_now() {
        let mut f = Fake::<Probe>::new(Net::default());
        f.add(Some(1), Some(Probe::default()));
        f.add(Some(1), Some(Probe::default()));
        assert_eq!(view(&f, 0).send(TO_ALL, 1, b"x", 0), E_STATE, "out of session");
        f.host();
        f.run(50.0);
        assert_eq!(view(&f, 0).send(TO_ALL, 1, b"x", 0), 0, "hosting alone: nobody");
        f.join(1);
        f.run(50.0);
        assert_eq!(view(&f, 0).send(1, 1, b"x", 0), E_STATE);
        f.run(300.0);
        assert_eq!(view(&f, 0).send(1, 1, b"x", 0), 1);
    }

    #[test]
    fn other_protocol_or_no_mod_receives_nothing() {
        let mut f = Fake::<Probe>::new(Net::default());
        f.add(Some(1), Some(Probe::default()));
        f.add(Some(2), Some(Probe::default()));
        f.add(None, None);
        f.host();
        f.join(1);
        f.join(2);
        f.run(400.0);
        assert!(f.hooks(0).seen.contains(&"ev8:1".to_string()));
        assert!(f.hooks(0).seen.contains(&"ev8:2".to_string()));
        assert_eq!(view(&f, 0).send(TO_ALL, 1, b"x", 0), 0);
        assert_eq!(view(&f, 0).peer_has_mod(1), 0);
    }

    #[test]
    fn rate_limit_refuses_and_sends_nothing() {
        let f = session(1);
        let v = view(&f, 0);
        let sent = (0..400).filter(|_| v.send(1, 1, b"x", 0) == 1).count();
        assert_eq!(sent, crate::RATE_MESSAGES_PER_S as usize);
        assert_eq!(f.stats(0).limited, 400 - sent as u64);
    }

    #[test]
    fn to_all_is_one_message_and_relay_is_free_for_the_host() {
        let mut f = Fake::<Probe>::new(Net { jitter_ms: 0.0, ..Net::default() });
        for _ in 0..4 {
            f.add(Some(1), Some(Probe::default()));
        }
        f.add(None, None);
        f.host();
        for g in 1..4 {
            f.join(g);
        }
        f.run(300.0);
        let v = view(&f, 1);
        assert_eq!(v.send(TO_ALL, 1, &[1; 100], 0), 3);
        assert_eq!((f.stats(1).sent, f.stats(1).sent_bytes), (1, 100), "every partner has the mod");
        f.join(4);
        f.run(300.0);
        assert_eq!(view(&f, 1).send(TO_ALL, 3, &[1; 100], 0), 3);
        assert_eq!((f.stats(1).sent, f.stats(1).sent_bytes), (2, 400), "one without: bytes per recipient");
        let host_sent = f.stats(0).sent;
        f.run(200.0);
        assert_eq!(f.stats(0).sent - host_sent, 0, "relaying guest to guest costs the host's mod nothing");
    }

    #[test]
    fn delivery_goes_on_out_of_the_world_and_a_stall_brings_it_at_once() {
        let mut f = session(1);
        f.set_world(1, false);
        f.run(50.0);
        let before = f.hooks(1).seen.len();
        assert_eq!(view(&f, 0).send(1, 3, b"menu", 0), 1);
        f.run(100.0);
        assert!(f.hooks(1).seen[before..].contains(&"msg3:0:menu".to_string()));
        f.stall(1, 1500.0);
        for _ in 0..30 {
            view(&f, 0).send(1, 3, b"x", 0);
            f.run(40.0);
        }
        let got = f.hooks(1).seen.iter().filter(|s| *s == "msg3:0:x").count();
        assert!(got <= 1, "nothing during the stall");
        f.run(400.0);
        assert_eq!(f.hooks(1).seen.iter().filter(|s| *s == "msg3:0:x").count(), 30, "all at once after it");
    }

    #[test]
    fn shared_state_late_joiner_refusal_and_end() {
        let mut f = Fake::<Probe>::new(Net::default());
        for _ in 0..3 {
            f.add(Some(1), Some(Probe::default()));
        }
        f.host();
        f.join(1);
        f.run(300.0);
        assert_eq!(view(&f, 0).state_set("room", b"k1"), crate::OK);
        assert_eq!(view(&f, 1).state_set("sys", b"evil"), crate::OK, "only a request");
        assert_eq!(view(&f, 1).state_set("note", b"n"), crate::OK);
        f.run(300.0);
        assert_eq!(f.state(1, "room").as_deref(), Some(&b"k1"[..]));
        assert_eq!(f.state(0, "sys"), None);
        assert_eq!(f.state(1, "note").as_deref(), Some(&b"n"[..]));
        f.join(2);
        f.run(400.0);
        assert_eq!(f.state(2, "room").as_deref(), Some(&b"k1"[..]), "late joiner");
        f.leave(0);
        f.run(50.0);
        assert!(f.hooks(2).seen.contains(&"state:room=None:0".to_string()));
        assert!(f.hooks(2).seen.contains(&"ev2:4294967295".to_string()));
    }

    #[test]
    fn denied_session_stall_and_shutdown() {
        let mut f = session(1);
        f.deny_session(true);
        assert_eq!(view(&f, 0).send(TO_ALL, 1, b"x", 0), E_DENIED);
        f.deny_session(false);
        let before = f.hooks(1).frames;
        f.stall(1, 1000.0);
        f.run(1000.0);
        assert!(f.hooks(1).frames - before <= 1);
        f.shutdown(1);
        let before = f.hooks(1).frames;
        f.run(100.0);
        assert_eq!(f.hooks(1).frames, before);
        assert_eq!(f.hooks(1).seen.last().map(String::as_str), Some("shutdown"));
    }

    #[test]
    fn clean_strips_what_it_must() {
        assert_eq!(clean("a\u{1b}[1m\u{202E}b\n\nc".as_bytes(), 0), "a[1mb c");
        assert_eq!(clean(&[b'x'; 300], 0).len(), 256);
        assert_eq!(clean(&[0xff, b'a'], 0), "\u{FFFD}a");
        assert_eq!(clean("éé".as_bytes(), 3), "é");
    }
}
