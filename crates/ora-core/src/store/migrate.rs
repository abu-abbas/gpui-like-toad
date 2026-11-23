//! One-time move of the per-user config folder from its old name
//! (`gpui-like-toad`) to the app name (`Kilat`).
//!
//! Nothing is ever deleted on failure: if a plain rename is not possible
//! (another volume, a locked file on Windows) the folder is copied to a
//! temporary sibling first and only renamed into place once the copy is
//! complete, so a half-copied `Kilat` folder can never be mistaken for the
//! real one. After a copy the old folder is left in place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What [`migrate_dir`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// The new folder exists already, or there is no old folder.
    NotNeeded,
    /// The old folder was renamed to the new name.
    Renamed,
    /// The old folder was copied; it is still there and can be removed by hand.
    Copied,
    /// Neither worked; the old folder is untouched (message for the log).
    Failed(String),
}

/// Move `old` to `new` if `old` exists and `new` does not.
pub fn migrate_dir(old: &Path, new: &Path) -> Migration {
    if new.exists() || !old.is_dir() {
        return Migration::NotNeeded;
    }
    if let Some(parent) = new.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Migration::Failed(format!("create {}: {e}", parent.display()));
        }
    }
    if fs::rename(old, new).is_ok() {
        return Migration::Renamed;
    }
    let tmp = sibling_tmp(new);
    let _ = fs::remove_dir_all(&tmp); // leftover of an earlier failed copy
    let result = copy_tree(old, &tmp).and_then(|()| fs::rename(&tmp, new));
    match result {
        Ok(()) => Migration::Copied,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Migration::Failed(format!("copy {} to {}: {e}", old.display(), new.display()))
        }
    }
}

fn sibling_tmp(new: &Path) -> PathBuf {
    let mut name = new.file_name().unwrap_or_default().to_os_string();
    name.push(".migrating");
    new.with_file_name(name)
}

pub(super) fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
        // symlinks and special files are not part of our data; skip
    }
    Ok(())
}

/// Run the migration for the real config folder. Call once, first thing
/// at startup. Returns a message only when something went wrong.
pub fn migrate_legacy_config_dir() -> Option<String> {
    match migrate_dir(&super::legacy_config_dir(), &super::config_dir()) {
        Migration::Failed(m) => Some(m),
        _ => None,
    }
}
