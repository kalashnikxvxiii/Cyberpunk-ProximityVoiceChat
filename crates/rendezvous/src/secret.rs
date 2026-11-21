//! Keys derived from a room name typed by hand.
//!
//! Room names are short, so they go through Argon2id: a server that wanted to
//! find the name behind a room id by trying them all would pay ~0.2 s and
//! 32 MiB per guess.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use vc_protocol::e2e::VoiceKey;

#[derive(Clone)]
pub struct Secret([u8; 32]);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

fn stretch(password: &str, salt: &[u8]) -> [u8; 32] {
    let params = Params::new(32 * 1024, 3, 1, Some(32)).expect("valid Argon2 parameters");
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .expect("Argon2 with valid parameters");
    out
}

fn normalize(s: &str) -> String {
    s.trim().to_ascii_uppercase()
}

impl Secret {
    /// A room typed by hand (manual mode, own server).
    pub fn for_room(room: &str) -> Self {
        Self(stretch(&normalize(room), b"vc-manual-room-v1"))
    }

    fn derive(&self, label: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(Some(b"vc-session-keys v1"), &self.0)
            .expand(label, &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        out
    }

    /// The room name servers see: unguessable, unrelated to the code.
    pub fn room_id(&self) -> String {
        self.derive(b"room")[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn voice_key(&self) -> VoiceKey {
        VoiceKey::from_secret(&self.derive(b"e2e"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_room_same_keys_other_room_other_keys() {
        let a = Secret::for_room(" nightcity");
        assert_eq!(a.room_id(), Secret::for_room("NIGHTCITY").room_id());
        assert_ne!(a.room_id(), Secret::for_room("nightcity2").room_id());
        assert_eq!(a.room_id().len(), 32);
    }
}
