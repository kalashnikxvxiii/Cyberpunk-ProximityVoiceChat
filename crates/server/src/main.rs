//! Voice relay. Clients join a room by code; voice is forwarded to everyone
//! in the same room who is within range of the speaker, or in the same party.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use vc_protocol::{
    ClientMsg, DEFAULT_PORT, ECHO_ID, MAX_PACKET, PlayerId, RosterEntry, ServerMsg, VERSION, Vec3,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PER_ROOM: usize = 16;

struct Config {
    bind: SocketAddr,
    range: f32,
    echo: bool,
}

struct Client {
    id: PlayerId,
    room: String,
    name: String,
    party: String,
    pos: Option<Vec3>,
    last_seen: Instant,
    /// Echo mode: where the client's own voice is played back from
    /// (where it first spoke).
    echo_anchor: Option<Vec3>,
}

struct Server {
    sock: UdpSocket,
    cfg: Config,
    clients: HashMap<SocketAddr, Client>,
    next_id: PlayerId,
    buf: Vec<u8>,
}

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\nusage: vc-server [--bind ADDR:PORT] [--range METRES] [--echo]");
            std::process::exit(2);
        }
    };
    let sock = UdpSocket::bind(cfg.bind).unwrap_or_else(|e| {
        eprintln!("bind {}: {e}", cfg.bind);
        std::process::exit(1);
    });
    sock.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    log(format_args!(
        "listening on {} (range {} m{})",
        cfg.bind,
        cfg.range,
        if cfg.echo { ", echo on" } else { "" }
    ));
    let mut server = Server { sock, cfg, clients: HashMap::new(), next_id: 1, buf: Vec::new() };
    server.run();
}

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config {
        bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
        range: 48.0,
        echo: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--bind" => cfg.bind = val()?.parse().map_err(|e| format!("--bind: {e}"))?,
            "--range" => cfg.range = val()?.parse().map_err(|e| format!("--range: {e}"))?,
            "--echo" => cfg.echo = true,
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    Ok(cfg)
}

fn log(args: std::fmt::Arguments<'_>) {
    println!("{args}");
}

fn dist2(a: Vec3, b: Vec3) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

impl Server {
    fn run(&mut self) -> ! {
        let mut packet = [0u8; MAX_PACKET + 1];
        let mut last_sweep = Instant::now();
        loop {
            match self.sock.recv_from(&mut packet) {
                Ok((n, from)) if n <= MAX_PACKET => {
                    if let Some(msg) = ClientMsg::decode(&packet[..n]) {
                        self.handle(from, msg);
                    }
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                // Windows reports ICMP "port unreachable" from earlier sends here.
                Err(_) => {}
            }
            if last_sweep.elapsed() >= Duration::from_secs(1) {
                last_sweep = Instant::now();
                self.sweep();
            }
        }
    }

    fn send(&mut self, to: SocketAddr, msg: &ServerMsg) {
        msg.encode(&mut self.buf);
        let _ = self.sock.send_to(&self.buf, to);
    }

    fn handle(&mut self, from: SocketAddr, msg: ClientMsg) {
        if let ClientMsg::Hello { version, room, name } = msg {
            return self.hello(from, version, room, name);
        }
        let Some(c) = self.clients.get_mut(&from) else {
            // Unknown sender (e.g. server restarted): ask it to say hello again.
            return self.send(from, &ServerMsg::Reject { reason: "not joined".into() });
        };
        c.last_seen = Instant::now();
        match msg {
            ClientMsg::Hello { .. } => unreachable!(),
            ClientMsg::Pose { pos } => c.pos = Some(pos),
            ClientMsg::Voice { seq, pos, opus } => {
                c.pos = Some(pos);
                // Anchor at the first word, not the first pose: while a save
                // loads the player can briefly report a placeholder position.
                if self.cfg.echo && c.echo_anchor.is_none() {
                    log(format_args!("[{}] {} echo anchored at {pos:?}", c.room, c.name));
                    c.echo_anchor = Some(pos);
                }
                self.relay(from, seq, pos, opus);
            }
            ClientMsg::Party { name } => {
                let room = c.room.clone();
                log(format_args!("[{room}] {} party -> '{name}'", c.name));
                c.party = name;
                self.broadcast_roster(&room);
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
        if let Some(c) = self.clients.get_mut(&from) {
            // Lost Welcome: answer again.
            c.last_seen = Instant::now();
            let id = c.id;
            return self.send(from, &ServerMsg::Welcome { id, range });
        }
        if self.clients.values().filter(|c| c.room == room).count() >= MAX_PER_ROOM {
            return self.send(from, &ServerMsg::Reject { reason: "room full".into() });
        }
        let id = self.alloc_id();
        log(format_args!("[{room}] {name} joined from {from} as #{id}"));
        self.clients.insert(
            from,
            Client {
                id,
                room: room.clone(),
                name,
                party: String::new(),
                pos: None,
                last_seen: Instant::now(),
                echo_anchor: None,
            },
        );
        self.send(from, &ServerMsg::Welcome { id, range });
        self.broadcast_roster(&room);
    }

    fn alloc_id(&mut self) -> PlayerId {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if id != ECHO_ID && !self.clients.values().any(|c| c.id == id) {
                return id;
            }
        }
    }

    fn relay(&mut self, from: SocketAddr, seq: u32, pos: Vec3, opus: Vec<u8>) {
        let speaker = &self.clients[&from];
        let range2 = self.cfg.range * self.cfg.range;
        let targets: Vec<(SocketAddr, bool)> = self
            .clients
            .iter()
            .filter(|(addr, c)| **addr != from && c.room == speaker.room)
            .filter_map(|(addr, c)| {
                let party = !speaker.party.is_empty() && c.party == speaker.party;
                let near = c.pos.is_some_and(|p| dist2(p, pos) <= range2);
                (party || near).then_some((*addr, party))
            })
            .collect();
        let id = speaker.id;
        let echo = speaker.echo_anchor;
        for (to, party) in targets {
            let msg = ServerMsg::Voice { from: id, party, seq, pos, opus: opus.clone() };
            self.send(to, &msg);
        }
        if let Some(anchor) = echo {
            let msg = ServerMsg::Voice { from: ECHO_ID, party: false, seq, pos: anchor, opus };
            self.send(from, &msg);
        }
    }

    fn broadcast_roster(&mut self, room: &str) {
        let mut members: Vec<(SocketAddr, RosterEntry)> = self
            .clients
            .iter()
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
        if let Some(c) = self.clients.remove(&addr) {
            log(format_args!("[{}] {} {why}", c.room, c.name));
            self.broadcast_roster(&c.room);
        }
    }

    fn sweep(&mut self) {
        let stale: Vec<SocketAddr> = self
            .clients
            .iter()
            .filter(|(_, c)| c.last_seen.elapsed() > TIMEOUT)
            .map(|(a, _)| *a)
            .collect();
        for addr in stale {
            self.remove(addr, "timed out");
        }
    }
}
