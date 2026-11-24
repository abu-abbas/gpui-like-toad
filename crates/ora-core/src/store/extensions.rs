//! Extensions: the optional parts of Kilat (SQLite, Redis, MailHog, later
//! other databases). Oracle is built in and always on.
//!
//! Stage 1 is an in-app registry only: every extension's code still ships
//! inside Kilat and "installing" just switches it on. The installed ids are
//! kept in `extensions.json`. A later stage can fetch signed extension
//! builds from GitHub Releases behind the same descriptors.

use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionKind {
    Database,
    Tool,
    /// Something done to the SQL editor's statement (Dry-run): no sidebar
    /// entry, a button in the editor toolbar.
    Action,
}

/// Static description of one extension.
#[derive(Debug, Clone, Copy)]
pub struct ExtensionInfo {
    /// Stable id, stored in `extensions.json`.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ExtensionKind,
    /// Cannot be uninstalled.
    pub built_in: bool,
    /// Version of the extension itself (bundled: the Kilat version).
    pub version: &'static str,
}

/// Every extension this build knows about, in display order.
pub const REGISTRY: &[ExtensionInfo] = &[
    ExtensionInfo {
        id: "oracle",
        name: "Oracle",
        description: "Connect to Oracle databases (Basic or TNS). Always on.",
        kind: ExtensionKind::Database,
        built_in: true,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "sqlite",
        name: "SQLite",
        description: "Open SQLite database files.",
        kind: ExtensionKind::Database,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "postgres",
        name: "PostgreSQL",
        description: "Connect to PostgreSQL servers.",
        kind: ExtensionKind::Database,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "mysql",
        name: "MySQL",
        description: "Connect to MySQL and MariaDB servers.",
        kind: ExtensionKind::Database,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "mssql",
        name: "SQL Server",
        description: "Connect to Microsoft SQL Server (SQL login).",
        kind: ExtensionKind::Database,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "redis",
        name: "Redis",
        description: "Browse keys, read values and run commands.",
        kind: ExtensionKind::Tool,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "dryrun",
        name: "Dry-run",
        description: "Run an UPDATE or DELETE without committing: see old and new values and get an undo script.",
        kind: ExtensionKind::Action,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
    ExtensionInfo {
        id: "mailhog",
        name: "MailHog",
        description: "Read the emails your app sends in development.",
        kind: ExtensionKind::Tool,
        built_in: false,
        version: env!("CARGO_PKG_VERSION"),
    },
];

pub fn extension_info(id: &str) -> Option<&'static ExtensionInfo> {
    REGISTRY.iter().find(|e| e.id == id)
}

impl DbKind {
    /// The extension that provides this database.
    pub fn extension_id(self) -> &'static str {
        self.key()
    }
}

impl ToolKind {
    /// The extension that provides this tool.
    pub fn extension_id(self) -> &'static str {
        self.key()
    }
}

/// The set of installed extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extensions {
    installed: BTreeSet<String>,
}

impl Default for Extensions {
    /// A fresh install: only the built-in extensions.
    fn default() -> Self {
        Self {
            installed: REGISTRY
                .iter()
                .filter(|e| e.built_in)
                .map(|e| e.id.to_string())
                .collect(),
        }
    }
}

impl Extensions {
    pub fn is_installed(&self, id: &str) -> bool {
        extension_info(id).is_some_and(|e| e.built_in) || self.installed.contains(id)
    }

    pub fn db_enabled(&self, kind: DbKind) -> bool {
        self.is_installed(kind.extension_id())
    }

    pub fn tool_enabled(&self, kind: ToolKind) -> bool {
        self.is_installed(kind.extension_id())
    }

    /// Whether any developer tool is installed.
    pub fn any_tool(&self) -> bool {
        REGISTRY
            .iter()
            .any(|e| e.kind == ExtensionKind::Tool && self.is_installed(e.id))
    }

    /// Switch an extension on. Unknown ids are ignored.
    pub fn install(&mut self, id: &str) {
        if extension_info(id).is_some() {
            self.installed.insert(id.to_string());
        }
    }

    /// Switch an extension off; built-in ones stay on.
    pub fn uninstall(&mut self, id: &str) {
        if !extension_info(id).is_some_and(|e| e.built_in) {
            self.installed.remove(id);
        }
    }

    /// Existing users keep what they already use: every extension that a
    /// saved connection or tool needs is switched on.
    pub fn with_in_use(mut self, profiles: &[SavedProfile], tools: &[ToolInstance]) -> Self {
        for p in profiles {
            self.install(p.kind.extension_id());
        }
        for t in tools {
            self.install(t.kind.extension_id());
        }
        self
    }

    fn to_json(&self) -> Json {
        let ids = self
            .installed
            .iter()
            .map(|id| Json::Str(id.clone()))
            .collect();
        Json::Obj(vec![("installed".into(), Json::Arr(ids))])
    }
}

const FILE: &str = "extensions.json";

/// Load the installed set. With no `extensions.json` yet (a fresh install,
/// or an upgrade from a version before extensions) the set starts from the
/// built-ins plus whatever the saved connections and tools use, and is
/// written back so the choice is stable afterwards.
pub fn load_extensions_in(dir: &Path) -> Extensions {
    if let Some(v) = read_json_file(&dir.join(FILE)) {
        let mut ext = Extensions::default();
        if let Some(items) = v.get("installed").and_then(Json::as_arr) {
            for id in items.iter().filter_map(Json::as_str) {
                ext.install(id);
            }
        }
        return ext;
    }
    let ext = Extensions::default().with_in_use(&load_profiles_in(dir), &load_tools_in(dir));
    // Best effort: failing to write only means the migration runs again.
    let _ = save_extensions_in(dir, &ext);
    ext
}

pub fn save_extensions_in(dir: &Path, ext: &Extensions) -> std::io::Result<()> {
    write_json_file(dir, FILE, &ext.to_json())
}

pub fn load_extensions() -> Extensions {
    load_extensions_in(&config_dir())
}

pub fn save_extensions(ext: &Extensions) -> std::io::Result<()> {
    save_extensions_in(&config_dir(), ext)
}

// ---------------------------------------------------------------------------
// Downloaded extensions
// ---------------------------------------------------------------------------
//
// Layout under the config dir:
//   extensions/<id>/<version>/<file>   the verified program
//   extensions/<id>/installed.json     {"version": "...", "file": "..."}
// The caller verifies the file (checksum and signature) before
// [`finish_remote_install`]; nothing here trusts the network.

/// A downloaded extension that is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInstall {
    pub id: String,
    pub version: String,
    pub file: String,
    pub path: PathBuf,
}

fn remote_root_in(dir: &Path) -> PathBuf {
    dir.join("extensions")
}

fn safe_part(part: &str) -> bool {
    !part.is_empty()
        && !part.starts_with('.')
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The installed version of a downloaded extension, if any.
pub fn remote_install_in(dir: &Path, id: &str) -> Option<RemoteInstall> {
    if !crate::extension_index::valid_id(id) {
        return None;
    }
    let ext_dir = remote_root_in(dir).join(id);
    let v = read_json_file(&ext_dir.join("installed.json"))?;
    let version = v.get("version")?.as_str()?.to_string();
    let file = v.get("file")?.as_str()?.to_string();
    if !safe_part(&version) || !safe_part(&file) {
        return None;
    }
    let path = ext_dir.join(&version).join(&file);
    path.is_file().then(|| RemoteInstall {
        id: id.to_string(),
        version,
        file,
        path,
    })
}

/// Move the verified `staged` file into place and record it. Older
/// versions of the same extension are removed afterwards. Nothing becomes
/// "installed" unless every step worked.
pub fn finish_remote_install_in(
    dir: &Path,
    id: &str,
    version: &str,
    file: &str,
    staged: &Path,
) -> std::io::Result<RemoteInstall> {
    use std::io::{Error, ErrorKind};
    if !crate::extension_index::valid_id(id) || !safe_part(version) || !safe_part(file) {
        return Err(Error::new(ErrorKind::InvalidInput, "unsafe extension name"));
    }
    let ext_dir = remote_root_in(dir).join(id);
    let version_dir = ext_dir.join(version);
    fs::create_dir_all(&version_dir)?;
    let target = version_dir.join(file);
    let part = version_dir.join(format!("{file}.part"));
    let copied = fs::copy(staged, &part).and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&part, fs::Permissions::from_mode(0o755))?;
        }
        fs::rename(&part, &target)
    });
    if let Err(err) = copied {
        let _ = fs::remove_file(&part);
        return Err(err);
    }
    write_json_file(
        &ext_dir,
        "installed.json",
        &Json::Obj(vec![
            ("version".into(), Json::Str(version.into())),
            ("file".into(), Json::Str(file.into())),
        ]),
    )?;
    if let Ok(entries) = fs::read_dir(&ext_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if entry.path().is_dir() && name.to_string_lossy() != version {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(RemoteInstall {
        id: id.to_string(),
        version: version.into(),
        file: file.into(),
        path: target,
    })
}

/// Delete a downloaded extension. Saved connections and tools are not
/// touched.
pub fn remove_remote_install_in(dir: &Path, id: &str) -> std::io::Result<()> {
    if !crate::extension_index::valid_id(id) {
        return Ok(());
    }
    match fs::remove_dir_all(remote_root_in(dir).join(id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn remote_install(id: &str) -> Option<RemoteInstall> {
    remote_install_in(&config_dir(), id)
}

pub fn finish_remote_install(
    id: &str,
    version: &str,
    file: &str,
    staged: &Path,
) -> std::io::Result<RemoteInstall> {
    finish_remote_install_in(&config_dir(), id, version, file, staged)
}

pub fn remove_remote_install(id: &str) -> std::io::Result<()> {
    remove_remote_install_in(&config_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("kilat-ext-{tag}-{}", new_profile_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_install_is_oracle_only() {
        let dir = temp_dir("fresh");
        let ext = load_extensions_in(&dir);
        assert!(ext.is_installed("oracle"));
        assert!(!ext.is_installed("sqlite"));
        assert!(!ext.any_tool());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_action_extension_does_not_open_the_developer_tools_section() {
        let mut ext = Extensions::default();
        ext.install("dryrun");
        assert!(ext.is_installed("dryrun"));
        assert!(!ext.any_tool());
    }

    #[test]
    fn upgrade_keeps_what_is_in_use() {
        let dir = temp_dir("upgrade");
        let mut tool = ToolInstance::new(ToolKind::Redis);
        tool.name = "r".into();
        save_tools_in(&dir, &[tool]).unwrap();
        let ext = load_extensions_in(&dir);
        assert!(ext.tool_enabled(ToolKind::Redis));
        assert!(!ext.tool_enabled(ToolKind::MailHog));
        // Stable: the choice is written, later loads read it.
        assert!(dir.join(FILE).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn install_uninstall_roundtrip_and_oracle_stays() {
        let dir = temp_dir("roundtrip");
        let mut ext = load_extensions_in(&dir);
        ext.install("sqlite");
        ext.uninstall("oracle");
        save_extensions_in(&dir, &ext).unwrap();
        let again = load_extensions_in(&dir);
        assert!(again.db_enabled(DbKind::Sqlite));
        assert!(again.db_enabled(DbKind::Oracle));
        ext.uninstall("sqlite");
        assert!(!ext.db_enabled(DbKind::Sqlite));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remote_install_replaces_old_versions_and_removes_cleanly() {
        let dir = temp_dir("remote");
        let staged = dir.join("staged.bin");
        fs::write(&staged, b"v1").unwrap();
        assert!(remote_install_in(&dir, "sqlite").is_none());
        let a = finish_remote_install_in(&dir, "sqlite", "1.0.0", "ext-sqlite", &staged).unwrap();
        assert_eq!(fs::read(&a.path).unwrap(), b"v1");
        fs::write(&staged, b"v2").unwrap();
        let b = finish_remote_install_in(&dir, "sqlite", "1.1.0", "ext-sqlite", &staged).unwrap();
        let now = remote_install_in(&dir, "sqlite").unwrap();
        assert_eq!(now.version, "1.1.0");
        assert_eq!(now.path, b.path);
        assert!(!a.path.exists(), "old version folder is removed");
        remove_remote_install_in(&dir, "sqlite").unwrap();
        assert!(remote_install_in(&dir, "sqlite").is_none());
        remove_remote_install_in(&dir, "sqlite").unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remote_install_refuses_unsafe_names() {
        let dir = temp_dir("unsafe");
        let staged = dir.join("s");
        fs::write(&staged, b"x").unwrap();
        assert!(finish_remote_install_in(&dir, "../x", "1.0.0", "f", &staged).is_err());
        assert!(finish_remote_install_in(&dir, "ok", "../1", "f", &staged).is_err());
        assert!(finish_remote_install_in(&dir, "ok", "1.0.0", "a/b", &staged).is_err());
        assert!(!dir.join("extensions").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
