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
    LEGACY_KEYCHAIN_SERVICE,
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
            "The system keychain is unavailable. Linux requires a Secret Service provider such as GNOME Keyring."
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
            Err(Error::NoEntry) => self.migrate_legacy(profile_id),
            Err(err) => Err(describe(err)),
        }
    }

    fn set(&self, profile_id: &str, password: &SecretString) -> Result<(), CredentialError> {
        entry(profile_id)?
            .set_password(password.expose_secret())
            .map_err(describe)
    }

    fn delete(&self, profile_id: &str) -> Result<(), CredentialError> {
        // Remove the pre-rename entry too, so an unticked "Save password"
        // does not leave a copy behind. Best effort: it may not exist.
        if let Ok(old) = Entry::new(LEGACY_KEYCHAIN_SERVICE, profile_id) {
            let _ = old.delete_credential();
        }
        match entry(profile_id)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(err) => Err(describe(err)),
        }
    }
}

impl OsKeychain {
    /// No entry under the new service: look under the pre-rename one and,
    /// if the password is there, copy it over. The old entry stays, so
    /// going back to an older build still works. A failure to read the old
    /// entry counts as "no password", never as an error.
    fn migrate_legacy(&self, profile_id: &str) -> Result<Option<SecretString>, CredentialError> {
        let Ok(old) = Entry::new(LEGACY_KEYCHAIN_SERVICE, profile_id) else {
            return Ok(None);
        };
        let Ok(pw) = old.get_password() else {
            return Ok(None);
        };
        let pw = SecretString::from(pw);
        // Best effort: if writing fails the password still works this time
        // and the copy is tried again next time.
        let _ =
            entry(profile_id).and_then(|e| e.set_password(pw.expose_secret()).map_err(describe));
        Ok(Some(pw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round trip against the real OS keychain. Ignored by default: CI and
    /// headless boxes usually have no keychain. Run with
    /// `cargo test -p kilat -- --ignored keychain`.
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
