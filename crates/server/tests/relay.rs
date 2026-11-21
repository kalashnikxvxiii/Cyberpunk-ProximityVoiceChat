//! Drives the relay in-process (and once as the real binary) the way the
//! plugin does.

use std::net::{SocketAddr, UdpSocket};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use vc_protocol::crypto::{KeyPair, Session};
use vc_protocol::e2e::VoiceKey;
use vc_protocol::{ClientMsg, Env, ServerMsg, VERSION, handshake};
use vc_server::{Config, Punches, Relay};

struct Running {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    punches: Punches,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn start(cfg: Config) -> Running {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let addr = sock.local_addr().unwrap();
    let mut relay = Relay::new(sock, KeyPair::generate(), cfg, Box::new(|_, _| {}));
    let punches = relay.punches();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::spawn(move || relay.run(&s));
    Running { addr, stop, punches, thread: Some(thread) }
}

struct Client {
    sock: UdpSocket,
    session: Session,
    buf: Vec<u8>,
}

impl Client {
    fn try_connect(addr: SocketAddr, psk: &[u8; 32]) -> Option<Self> {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let eph = KeyPair::generate();
        sock.send(&handshake::hello(&eph)).unwrap();
        let mut pkt = [0u8; 2048];
        let n = sock.recv(&mut pkt).ok()?;
        let (session, _) = handshake::accept(&eph, &pkt[..n], psk)?;
        Some(Self { sock, session, buf: Vec::new() })
    }

    fn connect(addr: SocketAddr) -> Self {
        Self::try_connect(addr, &[0; 32]).expect("handshake")
    }

    fn send(&mut self, msg: ClientMsg) {
        let mut plain = Vec::new();
        msg.encode(&mut plain);
        self.session.seal(&plain, &mut self.buf);
        self.sock.send(&self.buf).unwrap();
    }

    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        let mut pkt = [0u8; 2048];
        let mut plain = Vec::new();
        while let Ok(n) = self.sock.recv(&mut pkt) {
            if self.session.open(&pkt[..n], &mut plain).is_some() {
                out.extend(ServerMsg::decode(&plain));
            }
        }
        out
    }

    fn join(&mut self, room: &str, name: &str) -> Option<u16> {
        self.send(ClientMsg::Hello { version: VERSION, room: room.into(), name: name.into() });
        self.drain().iter().find_map(|m| match m {
            ServerMsg::Welcome { id, .. } => Some(*id),
            _ => None,
        })
    }
}

fn voices(msgs: &[ServerMsg]) -> Vec<(u16, bool)> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMsg::Voice { from, party, .. } => Some((*from, *party)),
            _ => None,
        })
        .collect()
}

fn voice(seq: u32, pos: Option<f32>) -> ClientMsg {
    ClientMsg::Voice { seq, pos: pos.map(|x| [x, 0.0, 0.0]), env: Env::default(), opus: b"OPUS".to_vec() }
}

fn pose(x: Option<f32>) -> ClientMsg {
    ClientMsg::Pose { pos: x.map(|x| [x, 0.0, 0.0]) }
}

#[test]
fn proximity_party_and_names() {
    let srv = start(Config::default());
    let (mut a, mut b) = (Client::connect(srv.addr), Client::connect(srv.addr));
    a.join("r", "alice\u{1b}[31m").unwrap();
    b.join("r", "bob").unwrap();
    a.drain();

    a.send(pose(Some(0.0)));
    b.send(pose(Some(10.0)));
    a.send(voice(1, Some(0.0)));
    assert_eq!(voices(&b.drain()), vec![(1, false)], "near");

    b.send(pose(Some(200.0))); // past where a voice fades out (72 m x 2.5)
    a.send(voice(2, Some(0.0)));
    assert!(voices(&b.drain()).is_empty(), "far");

    a.send(ClientMsg::Party { name: "squad".into() });
    b.send(ClientMsg::Party { name: "squad".into() });
    let names: Vec<String> = b
        .drain()
        .iter()
        .filter_map(|m| match m {
            ServerMsg::Roster { players } => {
                Some(players.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(","))
            }
            _ => None,
        })
        .collect();
    assert_eq!(names.last().unwrap(), "alice[31m,bob", "control chars stripped");
    a.drain();
    a.send(voice(3, Some(0.0)));
    assert_eq!(voices(&b.drain()), vec![(1, true)], "far but same party");
}

#[test]
fn lobby_hears_everybody() {
    let srv = start(Config::default());
    let (mut a, mut b) = (Client::connect(srv.addr), Client::connect(srv.addr));
    a.join("r", "a").unwrap();
    b.join("r", "b").unwrap();
    // b is far in the world, a is in the lobby: heard anyway.
    b.send(pose(Some(500.0)));
    a.send(pose(None));
    a.drain();
    a.send(voice(1, None));
    assert_eq!(voices(&b.drain()).len(), 1);
    // And the other way round.
    b.send(voice(1, Some(500.0)));
    assert_eq!(voices(&a.drain()).len(), 1);
}

#[test]
fn session_key_and_seats() {
    let psk = [7u8; 32];
    let seats = Arc::new(AtomicUsize::new(2));
    let srv = start(Config { psk, max_players: seats.clone(), ..Config::default() });
    assert!(Client::try_connect(srv.addr, &[0; 32]).is_none(), "outsider without the session key");
    let mut a = Client::try_connect(srv.addr, &psk).unwrap();
    let mut b = Client::try_connect(srv.addr, &psk).unwrap();
    let mut c = Client::try_connect(srv.addr, &psk).unwrap();
    assert!(a.join("s", "a").is_some());
    assert!(b.join("s", "b").is_some());
    assert!(c.join("s", "c").is_none(), "third seat in a two-player session");
    seats.store(3, Ordering::Relaxed);
    assert!(c.join("s", "c").is_some(), "seat opens when the session grows");
}

#[test]
fn punches_reach_listed_addresses_only() {
    let srv = start(Config::default());
    let target = UdpSocket::bind("127.0.0.1:0").unwrap();
    target.set_read_timeout(Some(Duration::from_millis(800))).unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger.set_read_timeout(Some(Duration::from_millis(800))).unwrap();
    srv.punches.add(target.local_addr().unwrap());
    let mut pkt = [0u8; 16];
    let (n, from) = target.recv_from(&mut pkt).unwrap();
    assert_eq!((&pkt[..n], from), (&[handshake::PUNCH][..], srv.addr));
    assert!(stranger.recv_from(&mut pkt).is_err());
}

#[test]
fn voice_is_opaque_to_the_relay() {
    let srv = start(Config::default());
    let (mut a, mut b) = (Client::connect(srv.addr), Client::connect(srv.addr));
    let id_a = a.join("r", "a").unwrap();
    b.join("r", "b").unwrap();
    let key = VoiceKey::from_secret(&[5; 32]);
    a.send(ClientMsg::Voice { seq: 9, pos: None, env: Env::default(), opus: key.seal(id_a, 9, b"secret") });
    let got = b.drain().into_iter().find_map(|m| match m {
        ServerMsg::Voice { from, seq, opus, .. } => Some((from, seq, opus)),
        _ => None,
    });
    let (from, seq, opus) = got.unwrap();
    assert_eq!(key.open(from, seq, &opus).unwrap(), b"secret");
    assert!(VoiceKey::from_secret(&[6; 32]).open(from, seq, &opus).is_none());
}

#[test]
fn re_handshake_does_not_inherit_the_player() {
    let srv = start(Config::default());
    let mut a = Client::connect(srv.addr);
    a.join("r", "alice").unwrap();
    let eph = KeyPair::generate();
    a.sock.send(&handshake::hello(&eph)).unwrap();
    let mut pkt = [0u8; 2048];
    let n = a.sock.recv(&mut pkt).unwrap();
    let (fresh, _) = handshake::accept(&eph, &pkt[..n], &[0; 32]).expect("handshake");
    // Until the new session is used, the old one keeps working.
    assert!(a.join("r", "alice").is_some());
    // Using the new session starts over: not joined.
    a.session = fresh;
    a.send(pose(Some(0.0)));
    assert!(matches!(a.drain().as_slice(), [ServerMsg::Reject { .. }]));
}

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[test]
fn binary_relays_and_rejects() {
    let port = 47_952;
    let key = std::env::temp_dir().join(format!("vc-test-{port}-{}.key", std::process::id()));
    let _ = std::fs::remove_file(&key);
    let child = Command::new(env!("CARGO_BIN_EXE_vc-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--key", key.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _srv = ServerProc(child);
    std::thread::sleep(Duration::from_millis(300));
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let (mut a, mut b) = (Client::connect(addr), Client::connect(addr));
    let id = a.join("r", "a").unwrap();
    b.join("r", "b").unwrap();
    a.drain();
    a.send(voice(1, Some(5.0)));
    assert_eq!(voices(&b.drain()), vec![(id, false)]);

    let raw = UdpSocket::bind("127.0.0.1:0").unwrap();
    raw.connect(addr).unwrap();
    raw.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    raw.send(&[handshake::HELLO, VERSION as u8, 0]).unwrap();
    let mut pkt = [0u8; 256];
    assert!(raw.recv(&mut pkt).is_err(), "short handshakes are ignored (no amplification)");
    let mut bad = handshake::hello(&KeyPair::generate());
    bad[1] = 99;
    raw.send(&bad).unwrap();
    let n = raw.recv(&mut pkt).unwrap();
    assert!(handshake::reject_reason(&pkt[..n]).unwrap().contains("protocol 99"));
    raw.send(&[vc_protocol::crypto::SEALED; 40]).unwrap();
    let n = raw.recv(&mut pkt).unwrap();
    assert_eq!(&pkt[..n], &[handshake::RESET]);
}
