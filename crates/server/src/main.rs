//! Standalone voice relay: `vc-server [--bind ADDR:PORT] [--range M] [--key FILE]`.

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vc_protocol::DEFAULT_PORT;
use vc_protocol::crypto::{self, KeyPair};
use vc_server::{Config, Level, Relay};

struct Args {
    bind: SocketAddr,
    range: f32,
    key: PathBuf,
}

fn main() {
    let args = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\nusage: vc-server [--bind ADDR:PORT] [--range METRES] [--key FILE]");
            std::process::exit(2);
        }
    };
    let key = load_or_create_key(&args.key).unwrap_or_else(|e| {
        eprintln!("key {}: {e}", args.key.display());
        std::process::exit(1);
    });
    let sock = UdpSocket::bind(args.bind).unwrap_or_else(|e| {
        eprintln!("bind {}: {e}", args.bind);
        std::process::exit(1);
    });
    sock.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    println!(
        "listening on {} (range {} m), key fingerprint {}",
        args.bind,
        args.range,
        crypto::fingerprint(&key.public)
    );
    let cfg = Config { range: args.range, ..Config::default() };
    let log = Box::new(|level: Level, msg: String| match level {
        Level::Info => println!("{msg}"),
        Level::Warn => println!("warning: {msg}"),
    });
    Relay::new(sock, key, cfg, log).run(&AtomicBool::new(false));
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
        range: vc_protocol::RANGE,
        key: PathBuf::from("vc-server.key"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut val = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--bind" => a.bind = val()?.parse().map_err(|e| format!("--bind: {e}"))?,
            "--range" => a.range = val()?.parse().map_err(|e| format!("--range: {e}"))?,
            "--key" => a.key = val()?.into(),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    Ok(a)
}

/// The server's identity; clients pin its fingerprint.
fn load_or_create_key(path: &PathBuf) -> std::io::Result<KeyPair> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let secret: [u8; 32] = bytes
                .try_into()
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "expected 32 bytes"))?;
            Ok(KeyPair::from_secret(secret))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = KeyPair::generate();
            write_private(path, &key.secret_bytes())?;
            println!("created new key {}", path.display());
            Ok(key)
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
