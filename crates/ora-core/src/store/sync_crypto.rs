//! Encryption for passwords that travel in the sync file. The key comes
//! from the user's sync passphrase (Argon2id with a random salt kept in
//! the file); a password is sealed with XChaCha20-Poly1305 bound to its
//! profile id.
//!
//! The nonce is derived from the key, the id and the password (a keyed
//! hash), so sealing the same password twice gives the same text. That is
//! what lets the merge tell "unchanged" from "changed" without a fresh
//! random value making every item look new on every sync. The only thing
//! it reveals is that two entries hold the same password.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use blake2::digest::{consts::U24, Mac};
use blake2::Blake2bMac;
use chacha20poly1305::aead::{rand_core::RngCore, Aead, OsRng, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};

pub struct SyncKey([u8; 32]);

impl Drop for SyncKey {
    fn drop(&mut self) {
        self.0 = [0; 32];
    }
}

pub fn new_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    salt
}

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text).ok()
}

pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<SyncKey, String> {
    // 19 MiB, 2 passes: the OWASP minimum for Argon2id. Runs in the
    // background a few times an hour, never on the UI thread.
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|e| e.to_string())?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| e.to_string())?;
    Ok(SyncKey(key))
}

fn nonce(key: &SyncKey, id: &str, plaintext: &str) -> [u8; 24] {
    let mut mac = <Blake2bMac<U24> as Mac>::new_from_slice(&key.0).expect("32-byte key");
    mac.update(id.as_bytes());
    mac.update(&[0]);
    mac.update(plaintext.as_bytes());
    mac.finalize().into_bytes().into()
}

/// Seal `plaintext` for `id`: base64 of nonce then ciphertext.
pub fn encrypt(key: &SyncKey, id: &str, plaintext: &str) -> String {
    let nonce_bytes = nonce(key, id, plaintext);
    let cipher = XChaCha20Poly1305::new((&key.0).into());
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext.as_bytes(),
                aad: id.as_bytes(),
            },
        )
        .expect("encrypting in memory cannot fail");
    let mut out = nonce_bytes.to_vec();
    out.extend(sealed);
    encode(&out)
}

/// `None` for a wrong key, another id, or damaged text.
pub fn decrypt(key: &SyncKey, id: &str, text: &str) -> Option<String> {
    let bytes = decode(text)?;
    if bytes.len() < 24 + 16 {
        return None;
    }
    let (nonce_bytes, sealed) = bytes.split_at(24);
    let plain = XChaCha20Poly1305::new((&key.0).into())
        .decrypt(
            XNonce::from_slice(nonce_bytes),
            Payload {
                msg: sealed,
                aad: id.as_bytes(),
            },
        )
        .ok()?;
    String::from_utf8(plain).ok()
}

const CHECK_ID: &str = "kilat-sync-check";

/// Stored next to the salt so a wrong passphrase is caught up front.
pub fn check_token(key: &SyncKey) -> String {
    encrypt(key, CHECK_ID, "kilat")
}

pub fn verify_check(key: &SyncKey, token: &str) -> bool {
    decrypt(key, CHECK_ID, token).as_deref() == Some("kilat")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_stable_and_bound_to_passphrase_and_id() {
        let salt = new_salt();
        let key = derive_key("correct horse", &salt).unwrap();
        let sealed = encrypt(&key, "p1", "s3cret");
        assert!(!sealed.contains("s3cret"));
        assert_eq!(sealed, encrypt(&key, "p1", "s3cret"));
        assert_ne!(sealed, encrypt(&key, "p2", "s3cret"));
        assert_eq!(decrypt(&key, "p1", &sealed).as_deref(), Some("s3cret"));
        assert_eq!(decrypt(&key, "p2", &sealed), None);

        let wrong = derive_key("battery staple", &salt).unwrap();
        assert_eq!(decrypt(&wrong, "p1", &sealed), None);
        assert!(verify_check(&key, &check_token(&key)));
        assert!(!verify_check(&wrong, &check_token(&key)));
        assert_eq!(decrypt(&key, "p1", "not base64!"), None);
    }
}
