//! End-to-end sealing of voice payloads with a key every member of a room
//! derives from the room's secret. Servers (yours, the host's, or a public
//! relay) forward the payload without being able to decode it.
//!
//! Payload: `[nonce 12][ciphertext][tag 16]`, with the speaker's id and the
//! packet's sequence number as associated data, so a server can't pass one
//! player's voice off as another's.

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::PlayerId;
use crate::crypto::random_bytes;

pub const OVERHEAD: usize = 12 + 16;

#[derive(Clone)]
pub struct VoiceKey(ChaCha20Poly1305);

fn aad(from: PlayerId, seq: u32) -> [u8; 6] {
    let mut a = [0u8; 6];
    a[..2].copy_from_slice(&from.to_le_bytes());
    a[2..].copy_from_slice(&seq.to_le_bytes());
    a
}

impl VoiceKey {
    pub fn from_secret(secret: &[u8; 32]) -> Self {
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(b"vc-e2e-voice v1"), secret)
            .expand(b"voice", &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        Self(ChaCha20Poly1305::new(&Key::from(key)))
    }

    pub fn seal(&self, from: PlayerId, seq: u32, opus: &[u8]) -> Vec<u8> {
        let nonce: [u8; 12] = random_bytes();
        let mut out = Vec::with_capacity(opus.len() + OVERHEAD);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(opus);
        let tag = self
            .0
            .encrypt_inout_detached(&Nonce::from(nonce), &aad(from, seq), out[12..].as_mut().into())
            .expect("message within ChaCha20 limits");
        out.extend_from_slice(&tag);
        out
    }

    pub fn open(&self, from: PlayerId, seq: u32, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < OVERHEAD {
            return None;
        }
        let nonce: [u8; 12] = payload[..12].try_into().ok()?;
        let (body, tag) = payload[12..].split_at(payload.len() - OVERHEAD);
        let tag = Tag::try_from(tag).ok()?;
        let mut out = body.to_vec();
        self.0
            .decrypt_inout_detached(&Nonce::from(nonce), &aad(from, seq), out.as_mut_slice().into(), &tag)
            .ok()?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_binding() {
        let k = VoiceKey::from_secret(&[9; 32]);
        let p = k.seal(3, 77, b"opus");
        assert_eq!(k.open(3, 77, &p).unwrap(), b"opus");
        assert!(k.open(4, 77, &p).is_none(), "re-attributed to another speaker");
        assert!(k.open(3, 78, &p).is_none(), "moved to another sequence number");
        assert!(VoiceKey::from_secret(&[8; 32]).open(3, 77, &p).is_none(), "other room");
    }
}
