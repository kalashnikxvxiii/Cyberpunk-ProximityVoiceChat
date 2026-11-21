//! Transport encryption between a client and the server.
//!
//! Handshake (plaintext): the client sends an ephemeral X25519 key; the server
//! answers with its static key, a fresh random nonce and a sealed empty
//! message that proves it owns the matching secret. Both sides derive two
//! ChaCha20-Poly1305 keys (one per direction) with HKDF-SHA256 over the DH
//! secret, both public keys and the nonce, so a replayed client hello never
//! recreates old keys (and their counters). An optional pre-shared key (the
//! NightCoop session's secret) is mixed in too: without it the server's proof
//! doesn't open and the client's packets are garbage to the server. Every later datagram is
//! `[SEALED][counter u64][ciphertext + tag]`, with the header as AAD and a
//! sliding window against replays.
//!
//! This is hop-by-hop: the server decrypts to route voice (it needs positions
//! and parties), then re-encrypts for each receiver.

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const SEALED: u8 = 0xE0;
pub const HEADER: usize = 1 + 8;
pub const TAG: usize = 16;
pub const OVERHEAD: usize = HEADER + TAG;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random generator");
    b
}

pub struct KeyPair {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Self {
        Self::from_secret(random_bytes())
    }

    pub fn from_secret(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }
}

/// Short human-checkable form of a public key, e.g. `K3QF-9X2M-TV7A-0D4R`.
pub fn fingerprint(public: &[u8; 32]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let hash = Sha256::digest(public);
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = String::new();
    for &b in &hash[..10] {
        bits = (bits << 8) | b as u32;
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            if !out.is_empty() && out.len() % 5 == 4 {
                out.push('-');
            }
            out.push(ALPHABET[((bits >> nbits) & 31) as usize] as char);
        }
    }
    out
}

pub struct Session {
    tx: ChaCha20Poly1305,
    rx: ChaCha20Poly1305,
    tx_counter: u64,
    replay: ReplayWindow,
}

impl Session {
    fn derive(
        shared: &[u8; 32],
        client_pub: &[u8; 32],
        server_pub: &[u8; 32],
        nonce: &[u8; 32],
        psk: &[u8; 32],
        is_client: bool,
    ) -> Self {
        let mut info = [0u8; 96];
        info[..32].copy_from_slice(client_pub);
        info[32..64].copy_from_slice(server_pub);
        info[64..].copy_from_slice(nonce);
        let mut okm = [0u8; 64];
        let mut salt = [0u8; 16 + 32];
        salt[..16].copy_from_slice(b"vc-voice-chat v4");
        salt[16..].copy_from_slice(psk);
        Hkdf::<Sha256>::new(Some(&salt), shared)
            .expand(&info, &mut okm)
            .expect("64 bytes is a valid HKDF-SHA256 length");
        let c2s = ChaCha20Poly1305::new(&Key::try_from(&okm[..32]).expect("32-byte key"));
        let s2c = ChaCha20Poly1305::new(&Key::try_from(&okm[32..]).expect("32-byte key"));
        let (tx, rx) = if is_client { (c2s, s2c) } else { (s2c, c2s) };
        Self { tx, rx, tx_counter: 0, replay: ReplayWindow::default() }
    }

    /// Client side, from the server's static key and handshake nonce.
    pub fn client(ephemeral: &KeyPair, server_pub: &[u8; 32], nonce: &[u8; 32], psk: &[u8; 32]) -> Self {
        let shared = ephemeral.secret.diffie_hellman(&PublicKey::from(*server_pub)).to_bytes();
        Self::derive(&shared, &ephemeral.public, server_pub, nonce, psk, true)
    }

    /// Server side, from the client's ephemeral key. `nonce` must be fresh
    /// random bytes for every handshake.
    pub fn server(server: &KeyPair, client_pub: &[u8; 32], nonce: &[u8; 32], psk: &[u8; 32]) -> Self {
        let shared = server.secret.diffie_hellman(&PublicKey::from(*client_pub)).to_bytes();
        Self::derive(&shared, client_pub, &server.public, nonce, psk, false)
    }

    /// Writes `[SEALED][counter][ciphertext + tag]` into `out`.
    pub fn seal(&mut self, plain: &[u8], out: &mut Vec<u8>) {
        let counter = self.tx_counter;
        self.tx_counter += 1;
        out.clear();
        out.push(SEALED);
        out.extend_from_slice(&counter.to_le_bytes());
        out.extend_from_slice(plain);
        let (header, body) = out.split_at_mut(HEADER);
        let tag = self
            .tx
            .encrypt_inout_detached(&nonce(counter), header, body.into())
            .expect("message within ChaCha20 limits");
        out.extend_from_slice(&tag);
    }

    /// Decrypts a sealed datagram; `None` if forged, corrupted or replayed.
    pub fn open(&mut self, packet: &[u8], out: &mut Vec<u8>) -> Option<()> {
        if packet.len() < OVERHEAD || packet[0] != SEALED {
            return None;
        }
        let counter = u64::from_le_bytes(packet[1..HEADER].try_into().ok()?);
        if !self.replay.check(counter) {
            return None;
        }
        let (header, rest) = packet.split_at(HEADER);
        let (body, tag) = rest.split_at(rest.len() - TAG);
        let tag = Tag::try_from(tag).ok()?;
        out.clear();
        out.extend_from_slice(body);
        self.rx
            .decrypt_inout_detached(&nonce(counter), header, out.as_mut_slice().into(), &tag)
            .ok()?;
        self.replay.accept(counter);
        Some(())
    }
}

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}

/// Accepts each counter once, tolerating reordering within 64 packets.
#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: u64,
}

impl ReplayWindow {
    fn check(&self, c: u64) -> bool {
        match self.highest {
            None => true,
            Some(h) if c > h => true,
            Some(h) => h - c < 64 && self.seen & (1 << (h - c)) == 0,
        }
    }

    fn accept(&mut self, c: u64) {
        match self.highest {
            Some(h) if c <= h => self.seen |= 1 << (h - c),
            Some(h) => {
                let shift = c - h;
                self.seen = if shift >= 64 { 0 } else { self.seen << shift };
                self.seen |= 1;
                self.highest = Some(c);
            }
            None => {
                self.highest = Some(c);
                self.seen = 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Session, Session) {
        let server = KeyPair::generate();
        let eph = KeyPair::generate();
        let nonce = random_bytes();
        let psk = [3; 32];
        (Session::client(&eph, &server.public, &nonce, &psk), Session::server(&server, &eph.public, &nonce, &psk))
    }

    #[test]
    fn wrong_psk_fails() {
        let server = KeyPair::generate();
        let eph = KeyPair::generate();
        let nonce = random_bytes();
        let mut c = Session::client(&eph, &server.public, &nonce, &[1; 32]);
        let mut s = Session::server(&server, &eph.public, &nonce, &[2; 32]);
        let (mut pkt, mut out) = (Vec::new(), Vec::new());
        c.seal(b"hi", &mut pkt);
        assert!(s.open(&pkt, &mut out).is_none());
    }

    #[test]
    fn replayed_hello_gets_new_keys() {
        let server = KeyPair::generate();
        let eph = KeyPair::generate();
        let (mut a, mut b) = (
            Session::server(&server, &eph.public, &random_bytes(), &[0; 32]),
            Session::server(&server, &eph.public, &random_bytes(), &[0; 32]),
        );
        let (mut pa, mut pb) = (Vec::new(), Vec::new());
        a.seal(b"same", &mut pa);
        b.seal(b"same", &mut pb);
        assert_ne!(pa, pb, "same counter and plaintext must not give the same ciphertext");
    }

    #[test]
    fn roundtrip_both_ways() {
        let (mut c, mut s) = pair();
        let (mut pkt, mut out) = (Vec::new(), Vec::new());
        c.seal(b"hello", &mut pkt);
        s.open(&pkt, &mut out).unwrap();
        assert_eq!(out, b"hello");
        s.seal(b"world", &mut pkt);
        c.open(&pkt, &mut out).unwrap();
        assert_eq!(out, b"world");
    }

    #[test]
    fn rejects_tamper_replay_and_wrong_key() {
        let (mut c, mut s) = pair();
        let (mut pkt, mut out) = (Vec::new(), Vec::new());
        c.seal(b"voice", &mut pkt);
        let mut bad = pkt.clone();
        bad[HEADER] ^= 1;
        assert!(s.open(&bad, &mut out).is_none());
        assert!(s.open(&pkt, &mut out).is_some());
        assert!(s.open(&pkt, &mut out).is_none(), "replay");
        let (_, mut other) = pair();
        c.seal(b"voice", &mut pkt);
        assert!(other.open(&pkt, &mut out).is_none());
    }

    #[test]
    fn reordering_within_window() {
        let (mut c, mut s) = pair();
        let mut out = Vec::new();
        let pkts: Vec<Vec<u8>> = (0..5)
            .map(|_| {
                let mut p = Vec::new();
                c.seal(b"x", &mut p);
                p
            })
            .collect();
        for i in [4, 1, 3, 0, 2] {
            assert!(s.open(&pkts[i], &mut out).is_some());
        }
        assert!(s.open(&pkts[2], &mut out).is_none());
    }

    #[test]
    fn fingerprint_format() {
        let f = fingerprint(&[7; 32]);
        assert_eq!(f.len(), 19);
        assert_eq!(f.matches('-').count(), 3);
    }
}
