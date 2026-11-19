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

mod history;
mod json;
mod layout;
mod profile_import;
mod profiles;
#[cfg(test)]
mod tests;
mod theme;
mod update_prefs;

pub use history::*;
pub use json::*;
pub use layout::*;
pub use profile_import::*;
pub use profiles::*;
pub use theme::*;
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

/// Per-user config directory for this app:
/// - macOS: `~/Library/Application Support/gpui-like-toad`
/// - Windows: `%APPDATA%\gpui-like-toad`
/// - Linux/other: `$XDG_CONFIG_HOME/gpui-like-toad` or `~/.config/gpui-like-toad`
pub fn config_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("gpui-like-toad")
    } else if cfg!(target_os = "windows") {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home_dir)
            .join("gpui-like-toad")
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("gpui-like-toad")
    }
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
