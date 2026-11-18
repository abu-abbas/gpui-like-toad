//! Saved connection passwords.
//!
//! Passwords never go into the profile file. When a profile has
//! `save_password` set, its password lives in the OS credential store
//! (macOS Keychain, Windows Credential Manager, Secret Service on Linux)
//! under service [`KEYCHAIN_SERVICE`] and account = the profile id. The
//! app crate supplies that store; this module holds the UI-free rules
//! (what to write, keep, or delete) behind the [`CredentialStore`] trait
//! so they can be tested without a real keychain.
//!
//! Passwords travel as [`SecretString`]: its `Debug` prints `[REDACTED]`
//! and the buffer is zeroed on drop, so a password does not end up in a
//! log line or linger in freed memory.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

pub use secrecy::{ExposeSecret, SecretString};

use crate::store::SavedProfile;

/// Service name of every keychain entry this app writes.
pub const KEYCHAIN_SERVICE: &str = "gpui-like-toad";

/// A keychain failure, as user-facing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialError(pub String);

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CredentialError {}

/// A place to keep passwords by profile id. Calls may block (a keychain
/// can show a system prompt), so callers run them off the UI thread.
pub trait CredentialStore: Send + Sync {
    /// The saved password, or `None` when there is no entry.
    fn get(&self, profile_id: &str) -> Result<Option<SecretString>, CredentialError>;
    fn set(&self, profile_id: &str, password: &SecretString) -> Result<(), CredentialError>;
    /// Remove the entry. Removing an entry that does not exist is `Ok`.
    fn delete(&self, profile_id: &str) -> Result<(), CredentialError>;
}

/// What saving the connection dialog does to the keychain entry.
pub enum PasswordAction {
    /// "Save password" ticked and a password typed: write it.
    Store(SecretString),
    /// Ticked but the field left blank: keep whatever is saved. The
    /// dialog never shows a saved password, so blank means "unchanged".
    Keep,
    /// Not ticked: remove any saved password.
    Delete,
}

impl PasswordAction {
    pub fn decide(save_password: bool, typed: SecretString) -> Self {
        if !save_password {
            PasswordAction::Delete
        } else if typed.expose_secret().is_empty() {
            PasswordAction::Keep
        } else {
            PasswordAction::Store(typed)
        }
    }

    pub fn apply(
        &self,
        store: &dyn CredentialStore,
        profile_id: &str,
    ) -> Result<(), CredentialError> {
        match self {
            PasswordAction::Store(pw) => store.set(profile_id, pw),
            PasswordAction::Keep => Ok(()),
            PasswordAction::Delete => store.delete(profile_id),
        }
    }
}

/// The password to connect with for a saved profile, if it has one. A
/// profile without `save_password` never touches the keychain.
pub fn saved_password(
    store: &dyn CredentialStore,
    profile: &SavedProfile,
) -> Result<Option<SecretString>, CredentialError> {
    if !profile.save_password {
        return Ok(None);
    }
    store.get(&profile.id)
}

/// In-memory store for tests and for running without an OS keychain.
/// Nothing is persisted.
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<String, SecretString>>,
}

impl MemoryCredentialStore {
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, profile_id: &str) -> Result<Option<SecretString>, CredentialError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| CredentialError("credential store poisoned".into()))?;
        Ok(entries
            .get(profile_id)
            .map(|pw| SecretString::from(pw.expose_secret().to_string())))
    }

    fn set(&self, profile_id: &str, password: &SecretString) -> Result<(), CredentialError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| CredentialError("credential store poisoned".into()))?;
        entries.insert(
            profile_id.to_string(),
            SecretString::from(password.expose_secret().to_string()),
        );
        Ok(())
    }

    fn delete(&self, profile_id: &str) -> Result<(), CredentialError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| CredentialError("credential store poisoned".into()))?;
        entries.remove(profile_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, save: bool) -> SavedProfile {
        SavedProfile {
            id: id.to_string(),
            name: "HR".to_string(),
            username: "hr".to_string(),
            connect_string: "db:1521/X".to_string(),
            folder: String::new(),
            access: Default::default(),
            save_password: save,
        }
    }

    fn pw(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn tick_and_type_stores_password() {
        let store = MemoryCredentialStore::default();
        PasswordAction::decide(true, pw("s3cret"))
            .apply(&store, "p1")
            .unwrap();
        let got = saved_password(&store, &profile("p1", true))
            .unwrap()
            .unwrap();
        assert_eq!(got.expose_secret(), "s3cret");
    }

    #[test]
    fn blank_field_keeps_saved_password() {
        let store = MemoryCredentialStore::default();
        store.set("p1", &pw("old")).unwrap();
        PasswordAction::decide(true, pw(""))
            .apply(&store, "p1")
            .unwrap();
        assert_eq!(store.get("p1").unwrap().unwrap().expose_secret(), "old");
        // Typing a new one replaces it.
        PasswordAction::decide(true, pw("new"))
            .apply(&store, "p1")
            .unwrap();
        assert_eq!(store.get("p1").unwrap().unwrap().expose_secret(), "new");
    }

    #[test]
    fn untick_deletes_password() {
        let store = MemoryCredentialStore::default();
        store.set("p1", &pw("old")).unwrap();
        PasswordAction::decide(false, pw("typed"))
            .apply(&store, "p1")
            .unwrap();
        assert!(store.is_empty());
        // Deleting again (nothing there) is fine.
        PasswordAction::decide(false, pw(""))
            .apply(&store, "p1")
            .unwrap();
    }

    #[test]
    fn profile_without_flag_never_reads_keychain() {
        let store = MemoryCredentialStore::default();
        store.set("p1", &pw("stale")).unwrap();
        assert!(saved_password(&store, &profile("p1", false))
            .unwrap()
            .is_none());
    }

    #[test]
    fn entries_are_per_profile_id() {
        let store = MemoryCredentialStore::default();
        store.set("a", &pw("1")).unwrap();
        store.set("b", &pw("2")).unwrap();
        store.delete("a").unwrap();
        assert!(store.get("a").unwrap().is_none());
        assert_eq!(store.get("b").unwrap().unwrap().expose_secret(), "2");
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = pw("hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
