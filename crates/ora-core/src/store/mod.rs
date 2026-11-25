//! On-disk persistence for saved connections, query history, and the
//! theme preference. Deliberately dependency-free: a tiny JSON value
//! type with a hand-written parser/serializer, and plain files under the
//! per-user config directory.
//!
//! All loads are fail-soft: a missing, unreadable, or corrupt file yields
//! the default (empty list / System theme) instead of an error, so a bad
//! file can never break startup. Saves return `io::Result` so the UI can
//! report a failure once, in the messages area.
//!
//! Passwords are NEVER written to these files. [`SavedProfile`] holds
//! only an id, the name, username, connect string, and a `save_password`
//! flag. When that flag is set the password lives in the OS keychain
//! under the profile id (see [`crate::credentials`]), never on disk here.

use crate::{ConnectionAccess, ConnectionEnvironment, DbKind};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

mod data_prefs;
mod extensions;
mod history;
mod json;
mod layout;
mod migrate;
mod profile_import;
mod profiles;
mod sync;
#[cfg(test)]
mod tests;
mod theme;
mod tools;
mod update_prefs;

pub use data_prefs::*;
pub use extensions::*;
pub use history::*;
pub use json::*;
pub use layout::*;
pub use migrate::*;
pub use profile_import::*;
pub use profiles::*;
pub use sync::*;
pub use theme::*;
pub use tools::*;
pub use update_prefs::*;

// ---------------------------------------------------------------------------
// Config directory
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Folder name before the rename to Kilat. Only used by the one-time
/// migration in [`migrate_legacy_config_dir`].
const LEGACY_DIR_NAME: &str = "gpui-like-toad";
const DIR_NAME: &str = "Kilat";

fn config_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir().join("Library").join("Application Support")
    } else if cfg!(target_os = "windows") {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home_dir)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
    }
}

/// Per-user config directory for this app:
/// - macOS: `~/Library/Application Support/Kilat`
/// - Windows: `%APPDATA%\Kilat`
/// - Linux/other: `$XDG_CONFIG_HOME/Kilat` or `~/.config/Kilat`
///
/// Installs from before the rename are moved over at startup, see
/// [`migrate_legacy_config_dir`].
pub fn config_dir() -> PathBuf {
    config_root().join(DIR_NAME)
}

/// The pre-rename folder (`gpui-like-toad`) next to [`config_dir`].
pub fn legacy_config_dir() -> PathBuf {
    config_root().join(LEGACY_DIR_NAME)
}

// ---------------------------------------------------------------------------
// File helpers
// ---------------------------------------------------------------------------

fn read_json_file(path: &Path) -> Option<Json> {
    let text = fs::read_to_string(path).ok()?;
    parse_json(&text).ok()
}

fn write_json_file(dir: &Path, file: &str, value: &Json) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join(file), to_json(value))
}
