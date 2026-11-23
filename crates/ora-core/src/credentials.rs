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

use crate::model::parse_easy_connect;
use crate::store::SavedProfile;
use crate::DbKind;

/// Service name of every keychain entry this app writes.
pub const KEYCHAIN_SERVICE: &str = "kilat";

/// Service name before the rename. Entries found here are copied to
/// [`KEYCHAIN_SERVICE`] on first read and left in place.
pub const LEGACY_KEYCHAIN_SERVICE: &str = "gpui-like-toad";

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

    /// Write the credential first and run `commit` (which makes the profile
    /// point at its new target) only when that succeeded. A keychain
    /// failure therefore never leaves a profile aimed at a target whose
    /// password is missing or still the one for the old target.
    pub fn apply_then<T>(
        &self,
        store: &dyn CredentialStore,
        profile_id: &str,
        commit: impl FnOnce() -> T,
    ) -> Result<T, CredentialError> {
        self.apply(store, profile_id)?;
        Ok(commit())
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

/// Whether a saved password may be used for `kind` / `username` /
/// `connect_string`, i.e. the form still points at the database and login
/// the password was saved for. A keychain entry is keyed by profile id
/// only, so without this check editing a profile's host, service or
/// username would send the old password to the new target.
///
/// Easy Connect strings compare by parts (host and service ignore case,
/// the port compares as a number), so `DB:01521/x` matches `db:1521/X`.
/// Anything else (TNS aliases, descriptors, SQLite paths) must match
/// exactly after trimming. Usernames must match exactly: a change in case
/// can be a different (quoted) Oracle user, so it asks again.
pub fn same_credential_target(
    saved: &SavedProfile,
    kind: DbKind,
    username: &str,
    connect_string: &str,
) -> bool {
    saved.kind == kind
        && saved.username.trim() == username.trim()
        && same_connect_target(&saved.connect_string, connect_string)
}

fn same_connect_target(a: &str, b: &str) -> bool {
    match (parse_easy_connect(a), parse_easy_connect(b)) {
        (Some((host_a, port_a, svc_a)), Some((host_b, port_b, svc_b))) => {
            host_a.eq_ignore_ascii_case(&host_b)
                && port_a.parse::<u32>().ok() == port_b.parse::<u32>().ok()
                && svc_a.eq_ignore_ascii_case(&svc_b)
        }
        (None, None) => a.trim() == b.trim(),
        _ => false,
    }
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
            kind: Default::default(),
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

    /// A store whose writes always fail.
    struct FailingStore;
    impl CredentialStore for FailingStore {
        fn get(&self, _: &str) -> Result<Option<SecretString>, CredentialError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &SecretString) -> Result<(), CredentialError> {
            Err(CredentialError("locked".into()))
        }
        fn delete(&self, _: &str) -> Result<(), CredentialError> {
            Ok(())
        }
    }

    #[test]
    fn apply_then_commits_only_after_credential_is_stored() {
        let store = MemoryCredentialStore::default();
        let mut committed_with_secret_present = false;
        let r = PasswordAction::decide(true, pw("new")).apply_then(&store, "p1", || {
            committed_with_secret_present = store.get("p1").unwrap().is_some();
        });
        assert!(r.is_ok());
        assert!(
            committed_with_secret_present,
            "secret must exist before commit"
        );
    }

    #[test]
    fn apply_then_skips_commit_when_keychain_fails() {
        let mut committed = false;
        let r = PasswordAction::decide(true, pw("new")).apply_then(&FailingStore, "p1", || {
            committed = true;
        });
        assert!(r.is_err());
        assert!(!committed, "profile must not move to the new target");
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

    #[test]
    fn credential_target_matches_same_database_and_user() {
        let saved = profile("p1", true);
        assert!(same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "db:1521/X"
        ));
        // Spelling differences of the same Easy Connect target.
        assert!(same_credential_target(
            &saved,
            DbKind::Oracle,
            " hr ",
            "DB:01521/x"
        ));
        assert!(same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "//db:1521/X"
        ));
    }

    #[test]
    fn credential_target_changes_with_host_port_service_or_user() {
        let saved = profile("p1", true);
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "other:1521/X"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "db:1522/X"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "db:1521/Y"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "db:1521"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "scott",
            "db:1521/X"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "HR",
            "db:1521/X"
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Sqlite,
            "hr",
            "db:1521/X"
        ));
        // Switching to a TNS alias or descriptor is a different target.
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "ORCL"
        ));
    }

    #[test]
    fn credential_target_tns_compares_exactly() {
        let mut saved = profile("p1", true);
        saved.connect_string = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521)))".into();
        assert!(same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            " (DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))) "
        ));
        assert!(!same_credential_target(
            &saved,
            DbKind::Oracle,
            "hr",
            "(DESCRIPTION=(ADDRESS=(HOST=evil)(PORT=1521)))"
        ));
    }
}
