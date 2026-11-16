//! The OS credential store behind [`ora_core::credentials::CredentialStore`]:
//! macOS Keychain, Windows Credential Manager, or the Secret Service
//! (GNOME Keyring / KWallet) on Linux, via the `keyring` crate.
//!
//! Entries are service [`KEYCHAIN_SERVICE`], account = profile id. Every
//! call can block (macOS may show an "allow access" prompt, Linux talks to
//! D-Bus), so callers run them on the DB worker or a background task,
//! never on the UI thread.

use keyring::{Entry, Error};
use ora_core::credentials::{
    CredentialError, CredentialStore, ExposeSecret as _, SecretString, KEYCHAIN_SERVICE,
};

pub struct OsKeychain;

/// The process-wide OS keychain.
pub fn store() -> &'static OsKeychain {
    static STORE: OsKeychain = OsKeychain;
    &STORE
}

fn entry(profile_id: &str) -> Result<Entry, CredentialError> {
    Entry::new(KEYCHAIN_SERVICE, profile_id).map_err(describe)
}

fn describe(err: Error) -> CredentialError {
    CredentialError(match err {
        Error::NoDefaultStore => {
            "Keychain OS tidak tersedia (di Linux butuh Secret Service, mis. GNOME Keyring)."
                .to_string()
        }
        Error::NoStorageAccess(e) | Error::PlatformFailure(e) => format!("Keychain error: {e}"),
        other => format!("Keychain error: {other}"),
    })
}

impl CredentialStore for OsKeychain {
    fn get(&self, profile_id: &str) -> Result<Option<SecretString>, CredentialError> {
        match entry(profile_id)?.get_password() {
            Ok(pw) => Ok(Some(SecretString::from(pw))),
            Err(Error::NoEntry) => Ok(None),
            Err(err) => Err(describe(err)),
        }
    }

    fn set(&self, profile_id: &str, password: &SecretString) -> Result<(), CredentialError> {
        entry(profile_id)?
            .set_password(password.expose_secret())
            .map_err(describe)
    }

    fn delete(&self, profile_id: &str) -> Result<(), CredentialError> {
        match entry(profile_id)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(err) => Err(describe(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round trip against the real OS keychain. Ignored by default: CI and
    /// headless boxes usually have no keychain. Run with
    /// `cargo test -p gpui-like-toad -- --ignored keychain`.
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let id = format!("test-{}", ora_core::store::new_profile_id());
        let ks = store();
        assert!(ks.get(&id).unwrap().is_none());
        ks.set(&id, &SecretString::from("s3cret".to_string()))
            .unwrap();
        assert_eq!(ks.get(&id).unwrap().unwrap().expose_secret(), "s3cret");
        ks.set(&id, &SecretString::from("new".to_string())).unwrap();
        assert_eq!(ks.get(&id).unwrap().unwrap().expose_secret(), "new");
        ks.delete(&id).unwrap();
        assert!(ks.get(&id).unwrap().is_none());
        // Deleting a missing entry is not an error.
        ks.delete(&id).unwrap();
    }
}
