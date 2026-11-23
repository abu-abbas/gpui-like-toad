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
        id: "redis",
        name: "Redis",
        description: "Browse keys, read values and run commands.",
        kind: ExtensionKind::Tool,
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
}
