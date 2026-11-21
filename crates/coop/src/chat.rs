//! The voice chat over NightCoop's channels: no network code of ours, no
//! addresses, nothing to set up.
//!
//! - The host draws a random room key for each session and keeps it in the
//!   mod's shared state ([`KEY`]); NightCoop hands it to every partner with the
//!   mod, late joiners included, and removes it when the session ends. Guests
//!   may write nothing.
//! - The audio thread queues Opus frames; every 40 ms one unreliable message
//!   carries what is queued (2 frames, 3 after `E_LIMIT`), sealed end to end
//!   with the room key and bound to the sender, so the host relaying guest to
//!   guest can't pass one player's voice off as another's. Silence sends nothing.
//! - The sender picks who hears: partners within [`RANGE`] (by their body in
//!   our world; unknown means everybody) and the same party at any distance.
//!   Positions are never sent: each side reads the other's body.
//! - After a stall (loading screens) only the newest ~100 ms is kept, on both
//!   sides, so latency doesn't pile up.
//!
//! Everything from a partner is untrusted: sizes, counts and the seal are
//! checked, replays dropped, and every sender has its own rate limit.

use std::collections::{HashMap, VecDeque};

use vc_protocol::e2e::VoiceKey;
use vc_protocol::{Env, Vec3};

use crate::{Coop, E_DENIED, E_LIMIT, HOST, Hooks, OK, Peer, SEND_RELIABLE, TO_ALL, UNRELIABLE_MAX, ev};

/// The mod's wire version in NightCoop's passport: data only flows between equal numbers.
pub const PROTOCOL: u32 = 3;
/// Shared state: the session's room key, 32 random bytes from the host.
pub const KEY: &str = "vc.key";
/// Distance at which a usual voice fades out, metres; shouting multiplies it ([`Env::reach`]).
pub const RANGE: f32 = vc_protocol::RANGE;

const VOICE: u16 = 1;
const PARTY: u16 = 2;
/// First byte of every message, bumped with the format.
const WIRE: u8 = 5;
const FRAME_MS: f32 = 20.0;
const BATCH: usize = 2;
const BATCH_LIMITED: usize = 3;
/// Frames kept waiting (100 ms); older ones are dropped.
const QUEUE_MAX: usize = 5;
const MAX_FRAMES: usize = 8;
const MAX_OPUS: usize = 400;
/// From this many listeners on, one `TO_ALL` (it counts once) and they filter by distance.
const TO_ALL_FROM: usize = 4;
/// A frame this long means a stall: what arrived meanwhile is stale.
const STALL_S: f32 = 0.25;
/// A gap this long in a speaker's frames restarts its playback.
const GAP_RESET: u32 = 5;
/// Per sender: voice messages per second, and the burst after a stall.
const IN_RATE: f64 = 50.0;
const IN_BURST: f64 = 100.0;
const PARTY_MAX: usize = 32;
/// Receivers' slack over [`RANGE`]: the two sides see positions a moment apart.
const RANGE_SLACK: f32 = 4.0;

/// An encoded 20 ms frame from the audio thread.
#[derive(Debug, Clone)]
pub struct Frame {
    pub seq: u32,
    pub opus: Vec<u8>,
    pub env: Env,
}

/// A frame for the audio thread.
#[derive(Debug, Clone)]
pub struct Heard {
    pub from: Peer,
    pub seq: u32,
    pub opus: Vec<u8>,
    pub env: Env,
    /// Their position in our world; `None`: not here (menus, lobby, loading).
    pub pos: Option<Vec3>,
    pub party: bool,
    /// Restart this speaker's playback (after a stall or a long gap).
    pub reset: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Member {
    pub peer: Peer,
    pub name: String,
    pub party: String,
    pub has_mod: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    /// One way, through `sharedTimeMs`, smoothed.
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub frames: u64,
    /// Frames missing in the sequence that the speaker did send.
    pub lost: u64,
    /// Frames the speaker never sent: their stalls, nobody in range, too big.
    pub skipped: u64,
    /// Stale, replayed or out of range.
    pub dropped: u64,
    /// Broken, forged or over the per-sender limit.
    pub refused: u64,
    /// `send` answered `E_LIMIT`.
    pub limited: u64,
    /// Our frames longer than [`STALL_S`].
    pub stalls: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub in_session: bool,
    pub is_host: bool,
    pub me: Peer,
    /// The room key is here: voice can flow.
    pub ready: bool,
    /// The host turned mods off for the session (`sdk_session`).
    pub denied: bool,
    pub members: Vec<Member>,
    pub metrics: Metrics,
}

/// What the chat needs from the game and the audio thread.
pub trait Game {
    /// Our position; `None` out of the world.
    fn me(&self) -> Option<Vec3>;
    /// Where an entity (EntityID hash) is in our world.
    fn body(&self, entity: u64) -> Option<Vec3>;
    /// Our party, empty for none.
    fn party(&self) -> String;
    /// Moves the frames encoded since the last call to `out`.
    fn take_frames(&mut self, out: &mut VecDeque<Frame>);
    fn heard(&mut self, h: Heard);
    /// Called on changes and once a second.
    fn status(&mut self, s: &Status);
}

#[derive(Default)]
struct Partner {
    party: String,
    last_seq: Option<u32>,
    tokens: f64,
    at: f64,
}

pub struct Chat<G: Game> {
    pub game: G,
    key: Option<VoiceKey>,
    session: bool,
    queue: VecDeque<Frame>,
    since_send_ms: f32,
    batch: usize,
    partners: HashMap<Peer, Partner>,
    /// Seconds, from the frames' `dt`.
    clock: f64,
    sent_party: Option<String>,
    /// The seq after our last voice message: what comes before the next one was never sent.
    next_seq: Option<u32>,
    pending: Vec<Heard>,
    status: Status,
    dirty: bool,
    refresh_s: f32,
}

fn distance(a: Vec3, b: Vec3) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// `a` comes after `b`, with wrapping.
fn after(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

impl<G: Game> Chat<G> {
    pub fn new(game: G) -> Self {
        Self {
            game,
            key: None,
            session: false,
            queue: VecDeque::new(),
            since_send_ms: 0.0,
            batch: BATCH,
            partners: HashMap::new(),
            clock: 0.0,
            sent_party: None,
            next_seq: None,
            pending: Vec::new(),
            status: Status::default(),
            dirty: true,
            refresh_s: 0.0,
        }
    }

    fn reset(&mut self) {
        self.key = None;
        self.queue.clear();
        self.batch = BATCH;
        self.partners.clear();
        self.sent_party = None;
        self.next_seq = None;
        self.pending.clear();
        self.status.denied = false;
        self.dirty = true;
    }

    fn body_of(&self, coop: &dyn Coop, peer: Peer) -> Option<Vec3> {
        match coop.partner_body(peer) {
            0 => None,
            e => self.game.body(e),
        }
    }

    fn same_party(&self, mine: &str, peer: Peer) -> bool {
        !mine.is_empty() && self.partners.get(&peer).is_some_and(|p| p.party == mine)
    }

    /// Follows the host's `sdk_session` switch through what NightCoop answers.
    fn note(&mut self, result: i32) {
        let denied = result == E_DENIED;
        if (denied || result >= 0) && denied != self.status.denied {
            self.status.denied = denied;
            self.dirty = true;
        }
    }

    fn load_key(&mut self, coop: &dyn Coop, is_host: bool) {
        if let Some(v) = coop.state_get(KEY).filter(|v| v.len() == 32) {
            self.set_key(&v);
        } else if is_host {
            let k: [u8; 32] = vc_protocol::crypto::random_bytes();
            let r = coop.state_set(KEY, &k);
            self.note(r);
            if r == OK {
                self.set_key(&k);
            }
        }
    }

    fn set_key(&mut self, v: &[u8]) {
        if let Ok(k) = <[u8; 32]>::try_from(v) {
            self.key = Some(VoiceKey::from_secret(&k));
            self.dirty = true;
        }
    }

    fn send_party(&mut self, coop: &dyn Coop, to: Peer) -> i32 {
        let party = self.game.party();
        let mut msg = vec![WIRE];
        msg.extend_from_slice(vc_protocol::clamp_str(&party).as_bytes());
        let r = coop.send(to, PARTY, &msg, SEND_RELIABLE);
        self.note(r);
        if r >= 0 && to == TO_ALL {
            self.sent_party = Some(party);
        }
        r
    }

    /// The loudest of the queued frames: the whole message carries that far.
    fn queued_env(&self) -> Option<Env> {
        let loud = self.queue.iter().map(|f| f.env.loud).max()?;
        Some(Env { loud, ..self.queue.back()?.env })
    }

    /// `[WIRE][seq u32] sealed([sent ms u32][skipped u32][env 5][n][(len u16, opus) × n])`,
    /// skipped: frames before seq we never sent, so listeners don't count them as lost.
    fn packet(&mut self, key: &VoiceKey, me: Peer, now_ms: u64) -> Option<Vec<u8>> {
        let me = u16::try_from(me).ok()?;
        loop {
            let first = self.queue.front()?;
            let seq = first.seq;
            let env = self.queued_env()?;
            let skipped = self.next_seq.map_or(0, |n| seq.wrapping_sub(n));
            let mut body = Vec::with_capacity(256);
            body.extend_from_slice(&(now_ms as u32).to_le_bytes());
            body.extend_from_slice(&skipped.to_le_bytes());
            body.extend_from_slice(&[env.wet, env.size, env.loud, env.ear, env.damp, self.queue.len() as u8]);
            for f in &self.queue {
                body.extend_from_slice(&(f.opus.len() as u16).to_le_bytes());
                body.extend_from_slice(&f.opus);
            }
            let mut out = vec![WIRE];
            out.extend_from_slice(&seq.to_le_bytes());
            out.extend_from_slice(&key.seal(me, seq, &body));
            if out.len() <= UNRELIABLE_MAX as usize {
                return Some(out);
            }
            self.queue.pop_front();
        }
    }

    fn send_voice(&mut self, coop: &dyn Coop, me: Peer, now_ms: u64) {
        let Some(key) = self.key.clone() else { return };
        let mine = self.game.party();
        let here = self.game.me();
        // As far as it fades out (past its range), not just its range.
        let range = RANGE * vc_protocol::FADE_TAIL * self.queued_env().unwrap_or_default().reach();
        let mut all = 0;
        let mut to = Vec::new();
        for p in coop.peers() {
            if coop.peer_has_mod(p) != 1 {
                continue;
            }
            all += 1;
            let near = match (here, self.body_of(coop, p)) {
                (Some(a), Some(b)) => distance(a, b) <= range,
                _ => true,
            };
            if near || self.same_party(&mine, p) {
                to.push(p);
            }
        }
        if to.is_empty() {
            self.queue.clear();
            return;
        }
        let Some(pkt) = self.packet(&key, me, now_ms) else { return };
        let results: Vec<i32> = if to.len() == all || to.len() >= TO_ALL_FROM {
            vec![coop.send(TO_ALL, VOICE, &pkt, 0)]
        } else {
            to.iter().map(|&p| coop.send(p, VOICE, &pkt, 0)).collect()
        };
        if results.contains(&E_LIMIT) {
            // Kept for the next message (listeners drop what they already have).
            self.status.metrics.limited += 1;
            if self.batch != BATCH_LIMITED {
                self.batch = BATCH_LIMITED;
                coop.log(crate::ffi::NCSDK_LOG_WARN as i32, "voice over the rate limit: 3 frames per message from now on");
            }
            return;
        }
        for r in results {
            self.note(r);
        }
        // shortcut: one counter for all listeners, so frames sent only to others while you were
        // out of range count as lost for you; per-listener counts if 3-player tests need them.
        if let Some(f) = self.queue.back() {
            self.next_seq = Some(f.seq.wrapping_add(1));
        }
        self.queue.clear();
    }

    fn voice(&mut self, coop: &dyn Coop, from: Peer, data: &[u8]) {
        let Some(key) = &self.key else { return };
        let (Some(&WIRE), Some(seq), Ok(from16)) = (data.first(), data.get(1..5), u16::try_from(from)) else {
            self.status.metrics.refused += 1;
            return;
        };
        let seq = u32::from_le_bytes(seq.try_into().unwrap_or_default());
        let clock = self.clock;
        let p = self.partners.entry(from).or_default();
        p.tokens = (p.tokens + (clock - p.at) * IN_RATE).min(IN_BURST);
        p.at = clock;
        if p.tokens < 1.0 {
            self.status.metrics.refused += 1;
            return;
        }
        p.tokens -= 1.0;
        let Some(body) = key.open(from16, seq, &data[5..]) else {
            self.status.metrics.refused += 1;
            return;
        };
        let Some((sent, skipped, env, frames)) = parse(&body) else {
            self.status.metrics.refused += 1;
            return;
        };
        let last = seq.wrapping_add(frames.len() as u32 - 1);
        let mut skip = 0;
        let mut reset = false;
        if let Some(prev) = p.last_seq {
            if !after(last, prev) {
                self.status.metrics.dropped += frames.len() as u64;
                return;
            }
            if after(seq, prev) {
                let gap = seq.wrapping_sub(prev) - 1;
                let skipped = skipped.min(gap);
                self.status.metrics.skipped += u64::from(skipped);
                self.status.metrics.lost += u64::from(gap - skipped);
                reset = gap > GAP_RESET;
            } else {
                skip = prev.wrapping_sub(seq) as usize + 1;
            }
        }
        p.last_seq = Some(last);
        let now = coop.where_().shared_time_ms;
        if now > 0 {
            let lat = (now as u32).wrapping_sub(sent) as i32;
            if (0..10_000).contains(&lat) {
                let m = &mut self.status.metrics;
                let lat = lat as f32;
                let first = m.frames == 0;
                m.jitter_ms += ((lat - m.latency_ms).abs() - m.jitter_ms) / 16.0;
                m.latency_ms = if first { lat } else { m.latency_ms + (lat - m.latency_ms) / 16.0 };
            }
        }
        let mine = self.game.party();
        let party = self.same_party(&mine, from);
        let pos = self.body_of(coop, from);
        if let (Some(a), Some(b)) = (self.game.me(), pos)
            && !party
            && distance(a, b) > RANGE * vc_protocol::FADE_TAIL * env.reach() + RANGE_SLACK
        {
            self.status.metrics.dropped += frames.len() as u64;
            return;
        }
        for (i, opus) in frames.into_iter().enumerate().skip(skip) {
            self.status.metrics.frames += 1;
            let seq = seq.wrapping_add(i as u32);
            self.pending.push(Heard { from, seq, opus: opus.to_vec(), env, pos, party, reset });
            reset = false;
        }
    }

    /// Our frame was a stall: per speaker only the newest frames, and a fresh start.
    fn trim_pending(&mut self) {
        let mut kept: HashMap<Peer, usize> = HashMap::new();
        let mut out = Vec::new();
        for h in self.pending.drain(..).rev() {
            let n = kept.entry(h.from).or_default();
            if *n < QUEUE_MAX {
                *n += 1;
                out.push(h);
            } else {
                self.status.metrics.dropped += 1;
            }
        }
        out.reverse();
        let mut seen = Vec::new();
        for h in &mut out {
            if !seen.contains(&h.from) {
                seen.push(h.from);
                h.reset = true;
            }
        }
        self.pending = out;
    }

    fn publish(&mut self, coop: &dyn Coop, dt: f32) {
        self.refresh_s += dt;
        if !self.dirty && self.refresh_s < 1.0 {
            return;
        }
        self.dirty = false;
        self.refresh_s = 0.0;
        let w = coop.where_();
        self.status.in_session = w.in_session;
        self.status.is_host = w.is_host;
        self.status.me = w.local_peer;
        self.status.ready = self.key.is_some();
        self.status.members = if w.in_session {
            coop.peers()
                .into_iter()
                .map(|p| Member {
                    peer: p,
                    name: coop.peer_name(p),
                    party: self.partners.get(&p).map(|x| x.party.clone()).unwrap_or_default(),
                    has_mod: coop.peer_has_mod(p) == 1,
                })
                .collect()
        } else {
            Vec::new()
        };
        self.game.status(&self.status);
    }
}

/// The sealed body: (sent ms, skipped, env, frames).
fn parse(b: &[u8]) -> Option<(u32, u32, Env, Vec<&[u8]>)> {
    let sent = u32::from_le_bytes(b.get(0..4)?.try_into().ok()?);
    let skipped = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
    let env = Env { wet: *b.get(8)?, size: *b.get(9)?, loud: *b.get(10)?, ear: *b.get(11)?, damp: *b.get(12)? };
    let n = *b.get(13)? as usize;
    if n == 0 || n > MAX_FRAMES {
        return None;
    }
    let mut at = 14;
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let len = u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize;
        at += 2;
        if len == 0 || len > MAX_OPUS {
            return None;
        }
        frames.push(b.get(at..at + len)?);
        at += len;
    }
    (at == b.len()).then_some((sent, skipped, env, frames))
}

impl<G: Game> Hooks for Chat<G> {
    fn frame(&mut self, coop: &dyn Coop, dt: f32) {
        let dt = dt.max(0.0);
        self.clock += f64::from(dt);
        let w = coop.where_();
        if w.in_session != self.session {
            self.session = w.in_session;
            self.reset();
        }
        if dt > STALL_S {
            self.status.metrics.stalls += 1;
            self.trim_pending();
        }
        for h in self.pending.drain(..) {
            self.game.heard(h);
        }
        self.game.take_frames(&mut self.queue);
        if !w.in_session {
            self.queue.clear();
            self.publish(coop, dt);
            return;
        }
        if self.key.is_none() {
            self.load_key(coop, w.is_host);
        }
        if self.sent_party.as_ref() != Some(&self.game.party()) {
            self.send_party(coop, TO_ALL);
        }
        let extra = self.queue.len().saturating_sub(QUEUE_MAX);
        self.queue.drain(..extra);
        self.status.metrics.dropped += extra as u64;
        self.since_send_ms += dt * 1000.0;
        let every = self.batch as f32 * FRAME_MS;
        if !self.queue.is_empty() && self.since_send_ms >= every {
            // On a 40 ms grid whatever the frame rate, without catching up after a stall.
            self.since_send_ms = (self.since_send_ms - every).min(every);
            self.send_voice(coop, w.local_peer, w.shared_time_ms);
        }
        self.publish(coop, dt);
    }

    fn event(&mut self, coop: &dyn Coop, event: i32, peer: Peer) {
        match event {
            ev::PEER_MOD => {
                self.send_party(coop, peer);
            }
            ev::PEER_LEAVE | ev::PEER_NO_MOD => {
                self.partners.remove(&peer);
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn message(&mut self, coop: &dyn Coop, from: Peer, channel: u16, data: &[u8]) {
        match (channel, data.split_first()) {
            (VOICE, _) => self.voice(coop, from, data),
            (PARTY, Some((&WIRE, name))) => {
                let name = coop.clean_text(name, PARTY_MAX);
                self.partners.entry(from).or_default().party = name;
                self.dirty = true;
            }
            _ => self.status.metrics.refused += 1,
        }
    }

    fn state_changed(&mut self, _: &dyn Coop, key: &str, data: Option<&[u8]>, by: Peer) {
        if key != KEY || by != HOST {
            return;
        }
        match data {
            Some(v) => self.set_key(v),
            None => {
                self.key = None;
                self.dirty = true;
            }
        }
    }

    /// Guests write nothing: the room key is the host's alone.
    fn state_request(&mut self, _: &dyn Coop, _: Peer, _: &str, _: &[u8]) -> bool {
        false
    }

    fn shutdown(&mut self) {
        self.session = false;
        self.reset();
        self.status = Status::default();
        self.game.status(&self.status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{Fake, Net};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Default)]
    struct World {
        pos: HashMap<Peer, Vec3>,
        party: HashMap<Peer, String>,
        now: f64,
    }

    struct TestGame {
        me: Peer,
        w: Rc<RefCell<World>>,
        talking: Rc<Cell<bool>>,
        next_seq: u32,
        next_at: f64,
        /// dB over the usual voice.
        loud: u8,
        /// (when, frame)
        heard: Vec<(f64, Heard)>,
        status: Status,
    }

    impl Game for TestGame {
        fn me(&self) -> Option<Vec3> {
            self.w.borrow().pos.get(&self.me).copied()
        }
        fn body(&self, entity: u64) -> Option<Vec3> {
            self.w.borrow().pos.get(&((entity - 0xB0D1_0000) as Peer)).copied()
        }
        fn party(&self) -> String {
            self.w.borrow().party.get(&self.me).cloned().unwrap_or_default()
        }
        /// An audio thread that encodes a frame every 20 ms (seq tells when it was made).
        fn take_frames(&mut self, out: &mut VecDeque<Frame>) {
            let now = self.w.borrow().now;
            while self.next_at + FRAME_MS as f64 <= now {
                self.next_at += FRAME_MS as f64;
                if self.talking.get() {
                    out.push_back(Frame { seq: self.next_seq, opus: vec![0xAB; 70], env: Env { wet: 1, size: 2, loud: self.loud, ear: 104, damp: 3 } });
                }
                self.next_seq += 1;
            }
        }
        fn heard(&mut self, h: Heard) {
            self.heard.push((self.w.borrow().now, h));
        }
        fn status(&mut self, s: &Status) {
            self.status = s.clone();
        }
    }

    struct Session {
        f: Fake<Chat<TestGame>>,
        w: Rc<RefCell<World>>,
        talk: Vec<Rc<Cell<bool>>>,
    }

    impl Session {
        fn new(players: usize, net: Net) -> Self {
            let w = Rc::new(RefCell::new(World::default()));
            let mut f = Fake::new(net);
            let mut talk = Vec::new();
            for i in 0..players {
                let t = Rc::new(Cell::new(false));
                let g = TestGame {
                    me: i as Peer,
                    w: w.clone(),
                    talking: t.clone(),
                    next_seq: 0,
                    next_at: 0.0,
                    loud: 0,
                    heard: Vec::new(),
                    status: Status::default(),
                };
                f.add(Some(PROTOCOL), Some(Chat::new(g)));
                talk.push(t);
            }
            f.host();
            for g in 1..players {
                f.join(g as Peer);
            }
            let mut s = Self { f, w, talk };
            s.run(500.0);
            s
        }

        fn run(&mut self, ms: f64) {
            // Small steps, so the test's audio clock follows the fake's.
            let end = self.f.now_ms() + ms;
            while self.f.now_ms() < end {
                let step = (end - self.f.now_ms()).min(5.0);
                self.f.run(step);
                self.w.borrow_mut().now = self.f.now_ms();
            }
        }

        fn game(&mut self, p: Peer) -> &mut TestGame {
            &mut self.f.hooks(p).game
        }

        fn heard_from(&mut self, at: Peer, from: Peer) -> Vec<(f64, Heard)> {
            self.game(at).heard.iter().filter(|(_, h)| h.from == from).cloned().collect()
        }
    }

    fn quiet() -> Net {
        Net { jitter_ms: 0.0, ..Net::default() }
    }

    #[test]
    fn the_host_draws_a_key_everybody_gets_it_and_a_new_session_a_new_one() {
        let mut s = Session::new(3, quiet());
        let k0 = s.f.state(0, KEY).expect("host's key");
        assert_eq!(k0.len(), 32);
        for p in 1..3 {
            assert_eq!(s.f.state(p, KEY).as_ref(), Some(&k0));
            assert!(s.game(p).status.ready);
        }
        assert!(s.game(0).status.is_host);
        assert_eq!(s.game(1).status.members.len(), 2);
        s.f.leave(0);
        s.run(100.0);
        assert!(!s.game(1).status.ready && !s.game(1).status.in_session);
        s.f.host();
        s.f.join(1);
        s.run(600.0);
        let k1 = s.f.state(1, KEY).expect("new key");
        assert_ne!(k0, k1);
        assert!(s.game(1).status.ready);
    }

    #[test]
    fn voice_reaches_host_and_guests_in_order() {
        let mut s = Session::new(3, quiet());
        s.talk[1].set(true);
        s.run(1000.0);
        s.talk[1].set(false);
        s.run(200.0);
        for at in [0, 2] {
            let got: Vec<u32> = s.heard_from(at, 1).iter().map(|(_, h)| h.seq).collect();
            assert!(got.len() >= 45, "{at}: {}", got.len());
            assert!(got.windows(2).all(|w| w[1] == w[0] + 1), "{got:?}");
            assert!(s.heard_from(at, 1).iter().all(|(_, h)| h.opus == vec![0xAB; 70] && h.env == Env { wet: 1, size: 2, loud: 0, ear: 104, damp: 3 }));
        }
        let m = &s.game(2).status.metrics;
        assert!(m.latency_ms > 40.0 && m.latency_ms < 120.0, "two hops: {m:?}");
        assert_eq!(s.f.stats(1).limited, 0);
    }

    #[test]
    fn late_joiner_and_peer_without_the_mod() {
        let w = quiet();
        let mut s = Session::new(2, w);
        s.f.add(None, None);
        s.f.join(2);
        s.talk[0].set(true);
        s.run(600.0);
        assert!(!s.heard_from(1, 0).is_empty());
        assert!(s.game(1).status.members.iter().any(|m| m.peer == 2 && !m.has_mod));
    }

    #[test]
    fn distance_and_party_pick_the_listeners() {
        let mut s = Session::new(3, quiet());
        {
            let mut w = s.w.borrow_mut();
            w.pos.insert(0, [0.0; 3]);
            w.pos.insert(1, [10.0, 0.0, 0.0]);
            w.pos.insert(2, [500.0, 0.0, 0.0]);
        }
        s.talk[0].set(true);
        s.run(500.0);
        assert!(!s.heard_from(1, 0).is_empty());
        assert!(s.heard_from(2, 0).is_empty(), "too far");
        for p in [0, 2] {
            s.w.borrow_mut().party.insert(p, "crew".into());
        }
        s.run(500.0);
        let far: Vec<_> = s.heard_from(2, 0);
        assert!(!far.is_empty() && far.iter().all(|(_, h)| h.party), "same party, any distance");
        assert!(s.heard_from(1, 0).iter().all(|(_, h)| !h.party));
    }

    #[test]
    fn a_shout_carries_further() {
        let mut s = Session::new(2, quiet());
        {
            let mut w = s.w.borrow_mut();
            w.pos.insert(0, [0.0; 3]);
            // Past where a usual voice fades out (72 m x 2.5).
            w.pos.insert(1, [200.0, 0.0, 0.0]);
        }
        s.talk[0].set(true);
        s.run(500.0);
        assert!(s.heard_from(1, 0).is_empty(), "a usual voice doesn't reach 200 m");
        s.game(0).loud = 12; // 4x: fades out about 720 m
        s.run(500.0);
        let got = s.heard_from(1, 0);
        assert!(!got.is_empty() && got.iter().all(|(_, h)| h.env.loud == 12));
    }

    #[test]
    fn unknown_positions_mean_everybody_hears() {
        let mut s = Session::new(2, quiet());
        s.w.borrow_mut().pos.insert(1, [900.0, 0.0, 0.0]);
        s.talk[0].set(true);
        s.run(500.0);
        let got = s.heard_from(1, 0);
        assert!(!got.is_empty() && got.iter().all(|(_, h)| h.pos.is_none()));
    }

    #[test]
    fn forged_replayed_or_broken_messages_are_dropped_without_a_panic() {
        let mut s = Session::new(3, quiet());
        let key = VoiceKey::from_secret(&s.f.state(0, KEY).unwrap().try_into().unwrap());
        let body = |n: u8| {
            let mut b = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, n];
            for _ in 0..n {
                b.extend_from_slice(&[3, 0, 1, 2, 3]);
            }
            b
        };
        let pkt = |from: u16, seq: u32, b: &[u8]| {
            let mut p = vec![WIRE];
            p.extend_from_slice(&seq.to_le_bytes());
            p.extend_from_slice(&key.seal(from, seq, b));
            p
        };
        let good = pkt(1, 100, &body(2));
        s.f.inject(2, 1, VOICE, &good);
        s.run(50.0);
        assert_eq!(s.heard_from(2, 1).len(), 2);
        s.f.inject(2, 1, VOICE, &good);
        s.f.inject(2, 0, VOICE, &good);
        s.f.inject(2, 1, VOICE, &pkt(1, 50, &body(1)));
        s.f.inject(2, 1, VOICE, &pkt(1, 200, &body(9)));
        s.f.inject(2, 1, VOICE, &pkt(1, 200, &[0; 13]));
        s.f.inject(2, 1, PARTY, &[9, b'x']);
        s.run(50.0);
        assert_eq!(s.heard_from(2, 1).len(), 2, "replay, re-attributed, old, too many frames, empty");
        assert!(s.heard_from(2, 0).is_empty());
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for i in 0..5000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 1100) as usize;
            let mut junk: Vec<u8> = (0..len).map(|j| (x >> (j % 56)) as u8).collect();
            if i % 2 == 0 && !junk.is_empty() {
                junk[0] = WIRE;
            }
            s.f.inject(2, (x % 4) as Peer, (x % 4) as u16, &junk);
            if i % 100 == 0 {
                s.run(20.0);
            }
        }
        s.run(50.0);
        assert_eq!(s.heard_from(2, 1).len(), 2);
        assert!(s.game(2).status.metrics.refused > 0);
        assert!(!s.f.inject_state_request(1, KEY, &[7; 32]), "guests can't change the key");
        assert!(!s.f.inject_state_request(1, "anything", b"x"));
    }

    #[test]
    fn a_long_frame_keeps_only_the_newest_audio() {
        let mut s = Session::new(2, quiet());
        s.talk[0].set(true);
        s.run(500.0);
        s.f.stall(1, 1500.0);
        s.run(1600.0);
        s.run(300.0);
        let heard = s.heard_from(1, 0);
        let mut by_time: HashMap<u64, usize> = HashMap::new();
        for (t, _) in &heard {
            *by_time.entry(*t as u64).or_default() += 1;
        }
        assert!(by_time.values().all(|&n| n <= QUEUE_MAX), "{by_time:?}");
        assert!(heard.iter().any(|(_, h)| h.reset));
        // Right after, the newest frame heard is fresh: latency didn't pile up.
        let (t, h) = heard.last().unwrap();
        let made = (h.seq + 1) as f64 * FRAME_MS as f64;
        assert!(t - made < 120.0, "{}", t - made);
    }

    #[test]
    fn a_stalled_sender_sends_only_its_newest_frames() {
        let mut s = Session::new(2, quiet());
        s.talk[0].set(true);
        s.run(400.0);
        s.f.stall(0, 1000.0);
        s.run(1200.0);
        let seqs: Vec<u32> = s.heard_from(1, 0).iter().map(|(_, h)| h.seq).collect();
        let jump = seqs.windows(2).map(|w| w[1] - w[0]).max().unwrap();
        assert!(jump > 40, "the stale second was dropped: {jump}");
        assert!(s.heard_from(1, 0).iter().any(|(_, h)| h.reset));
        s.run(1100.0); // status is published once a second
        let m = &s.game(1).status.metrics;
        assert_eq!(m.lost, 0, "never sent is not lost: {m:?}");
        assert!(m.skipped > 40, "{m:?}");
    }

    /// Four talkers, 30/60/variable fps, 0-10 % loss: no limit hit, little added latency.
    #[test]
    fn four_talkers_stay_under_the_limits_with_low_added_latency() {
        for (fps, fps_jitter, loss) in [(60.0, 0.0, 0.0), (30.0, 0.0, 0.05), (45.0, 25.0, 0.10), (144.0, 2.0, 0.02)] {
            let net = Net { latency_ms: 30.0, jitter_ms: 10.0, loss, ..Net::default() };
            let mut s = Session::new(4, net);
            for p in 0..4 {
                s.f.set_fps(p, fps, fps_jitter);
                s.talk[p as usize].set(true);
            }
            s.run(10_000.0);
            for p in 0..4 {
                assert_eq!(s.f.stats(p).limited, 0, "fps {fps}: {p} hit the limit");
                assert_eq!(s.f.stats(p).dropped_in, 0, "fps {fps}: {p} over an incoming limit");
            }
            // Over a direct path (one hop, 30-40 ms), at the 95th percentile.
            let mut added: Vec<f64> = Vec::new();
            for at in 0..4 {
                for from in 0..4 {
                    if at == from {
                        continue;
                    }
                    let hops = if at != 0 && from != 0 { 2.0 } else { 1.0 };
                    for (t, h) in s.heard_from(at, from) {
                        let made = (h.seq + 1) as f64 * FRAME_MS as f64;
                        added.push(t - made - hops * 40.0);
                    }
                }
            }
            added.sort_by(f64::total_cmp);
            let p95 = added[added.len() * 95 / 100];
            // The 40 ms batch plus one game frame of the listener: what NightCoop's channels cost
            // (measured: 60 fps 40 ms, 30 fps 70, 45±25 fps 60, 144 fps 25).
            let bound = 40.0 + 1000.0 / fps + fps_jitter;
            assert!(p95 <= bound, "fps {fps} loss {loss}: p95 added latency {p95:.0} ms over {bound:.0}");
            let m = s.game(1).status.metrics.clone();
            assert!(loss > 0.0 || m.lost == 0, "{m:?}");
        }
    }

    #[test]
    fn denied_session_is_reported_and_shutdown_stops_everything() {
        let mut s = Session::new(2, quiet());
        s.f.deny_session(true);
        s.talk[0].set(true);
        s.run(300.0);
        assert!(s.game(0).status.denied);
        s.f.deny_session(false);
        s.run(300.0);
        assert!(!s.game(0).status.denied);
        s.f.shutdown(0);
        assert_eq!(s.game(0).status, Status::default());
    }
}
