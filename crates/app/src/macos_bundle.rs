use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use rustix::fs::{renameat_with, RenameFlags, CWD};

/// Older updaters replace the bundle contents at the original path.
pub(crate) fn relaunch_from_renamed_bundle() -> bool {
    let Ok(original) = gpui_updater_core::current_install_root() else {
        return false;
    };
    let renamed = match rename_legacy_bundle(&original) {
        Ok(Some(path)) => path,
        Ok(None) => return false,
        Err(err) => {
            eprintln!("Could not rename the legacy macOS bundle: {err}");
            return false;
        }
    };

    // Launch Services registers the bundle at its new path. Force a new
    // instance because this process is still running with the same bundle ID.
    match Command::new("/usr/bin/open")
        .arg("-n")
        .arg(&renamed)
        .status()
    {
        Ok(status) if status.success() => return true,
        result => eprintln!("Could not reopen Kilat after renaming the bundle: {result:?}"),
    }
    if let Err(err) = rename_exclusively(&renamed, &original) {
        eprintln!("Could not restore the original macOS bundle path: {err}");
    }
    false
}

fn rename_legacy_bundle(original: &Path) -> io::Result<Option<PathBuf>> {
    if original.file_name().and_then(|name| name.to_str()) != Some("Ora The Explorer.app")
        || !original.is_dir()
    {
        return Ok(None);
    }
    let renamed = original.with_file_name(concat!(app_name!(), ".app"));
    match rename_exclusively(original, &renamed) {
        Ok(()) => Ok(Some(renamed)),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(err) => Err(err),
    }
}

fn rename_exclusively(source: &Path, target: &Path) -> io::Result<()> {
    renameat_with(CWD, source, CWD, target, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn renames_the_legacy_bundle_once() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Ora The Explorer.app");
        let new = dir.path().join("Kilat.app");
        fs::create_dir_all(old.join("Contents")).unwrap();
        fs::write(old.join("Contents/Info.plist"), b"bundle metadata").unwrap();

        assert_eq!(rename_legacy_bundle(&old).unwrap(), Some(new.clone()));
        assert!(!old.exists());
        assert_eq!(
            fs::read(new.join("Contents/Info.plist")).unwrap(),
            b"bundle metadata"
        );
        assert_eq!(rename_legacy_bundle(&new).unwrap(), None);
    }

    #[test]
    fn leaves_both_copies_when_kilat_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Ora The Explorer.app");
        let new = dir.path().join("Kilat.app");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&new).unwrap();

        assert_eq!(rename_legacy_bundle(&old).unwrap(), None);
        assert!(old.is_dir());
        assert!(new.is_dir());
    }
}
