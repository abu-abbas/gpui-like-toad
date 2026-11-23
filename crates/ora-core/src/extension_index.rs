//! The signed index of downloadable extensions (`extensions.json` on the
//! GitHub release tagged [`INDEX_TAG`]). Pure logic: parsing, validation
//! and picking the right file for this machine. Fetching and signature
//! checks live in the app crate; nothing here trusts the text it parses
//! beyond what it validates.
//!
//! ```json
//! { "schema": 1, "revision": 7,
//!   "extensions": [{
//!     "id": "sqlite", "name": "SQLite", "description": "…",
//!     "kind": "database", "version": "1.0.0",
//!     "min_app": "0.1.51", "max_app": "0.2.0",
//!     "tag": "ext-sqlite-1.0.0",
//!     "files": { "macos-aarch64": "kilat-ext-sqlite-1.0.0-macos-aarch64", … }
//!   }] }
//! ```

use crate::store::{parse_json, ExtensionKind, Json};
use crate::update::Version;

/// Release that carries `extensions.json` and `extensions.json.minisig`.
pub const INDEX_TAG: &str = "extensions-index";
pub const INDEX_FILE: &str = "extensions.json";
const SCHEMA: u64 = 1;

/// `<os>-<arch>` as used in the index, e.g. `macos-aarch64`.
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Ids become folder names and tags, so only `[a-z0-9-]` (not starting
/// with `-`) is accepted.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ExtensionKind,
    pub version: Version,
    pub min_app: Version,
    pub max_app: Option<Version>,
    /// Release tag holding the files: `ext-<id>-<version>`.
    pub tag: String,
    /// Platform key (`macos-aarch64`) to release file name.
    pub files: Vec<(String, String)>,
}

impl IndexEntry {
    pub fn compatible_with(&self, app: Version) -> bool {
        app >= self.min_app && self.max_app.is_none_or(|max| app <= max)
    }

    /// The file for `platform`, if this extension ships one.
    pub fn file_for(&self, platform: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(p, _)| p == platform)
            .map(|(_, f)| f.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionIndex {
    pub revision: u64,
    pub entries: Vec<IndexEntry>,
}

impl ExtensionIndex {
    /// Parse and validate. Entries that break the rules (bad id, a tag
    /// that is not `ext-<id>-<version>`, unsafe file names) are dropped
    /// one by one; a wrong schema or unreadable JSON fails the whole index.
    pub fn parse(text: &str) -> Result<Self, String> {
        let json = parse_json(text).map_err(|_| "The extension index is not valid JSON.")?;
        if json.get("schema").and_then(Json::as_u64) != Some(SCHEMA) {
            return Err("The extension index has an unsupported format.".into());
        }
        let revision = json
            .get("revision")
            .and_then(Json::as_u64)
            .ok_or("The extension index has no revision.")?;
        let entries = json
            .get("extensions")
            .and_then(Json::as_arr)
            .unwrap_or_default()
            .iter()
            .filter_map(parse_entry)
            .collect();
        Ok(Self { revision, entries })
    }

    pub fn get(&self, id: &str) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

fn parse_entry(v: &Json) -> Option<IndexEntry> {
    let text = |k: &str| v.get(k).and_then(Json::as_str);
    let id = text("id").filter(|id| valid_id(id))?.to_string();
    let version = Version::parse(text("version")?)?;
    let tag = text("tag")?.to_string();
    if tag != format!("ext-{id}-{version}") {
        return None;
    }
    let kind = match text("kind")? {
        "database" => ExtensionKind::Database,
        "tool" => ExtensionKind::Tool,
        _ => return None,
    };
    let files = match v.get("files")? {
        Json::Obj(pairs) => pairs
            .iter()
            .filter_map(|(platform, file)| {
                let file = file.as_str()?;
                (valid_id(&platform.replace('_', "-")) && valid_file_name(file))
                    .then(|| (platform.clone(), file.to_string()))
            })
            .collect::<Vec<_>>(),
        _ => return None,
    };
    if files.is_empty() {
        return None;
    }
    Some(IndexEntry {
        name: text("name").unwrap_or(&id).chars().take(60).collect(),
        description: text("description")
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        kind,
        min_app: text("min_app")
            .and_then(Version::parse)
            .unwrap_or(Version(0, 0, 0)),
        max_app: text("max_app").and_then(Version::parse),
        id,
        version,
        tag,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"schema":1,"revision":3,"extensions":[
      {"id":"sqlite","name":"SQLite","description":"Open files","kind":"database",
       "version":"1.0.0","min_app":"0.1.51","tag":"ext-sqlite-1.0.0",
       "files":{"macos-aarch64":"kilat-ext-sqlite-1.0.0-macos-aarch64",
                "linux-x86_64":"kilat-ext-sqlite-1.0.0-linux-x86_64"}},
      {"id":"../evil","kind":"tool","version":"1.0.0","tag":"ext-../evil-1.0.0","files":{"linux-x86_64":"x"}},
      {"id":"redis","kind":"tool","version":"1.0.0","tag":"ext-redis-9.9.9","files":{"linux-x86_64":"x"}},
      {"id":"mailhog","kind":"tool","version":"1.0.0","tag":"ext-mailhog-1.0.0","files":{"linux-x86_64":"../x"}}
    ]}"#;

    #[test]
    fn keeps_only_valid_entries() {
        let idx = ExtensionIndex::parse(GOOD).unwrap();
        assert_eq!(idx.revision, 3);
        assert_eq!(idx.entries.len(), 1);
        let e = idx.get("sqlite").unwrap();
        assert_eq!(e.version, Version(1, 0, 0));
        assert!(e
            .file_for("linux-x86_64")
            .unwrap()
            .ends_with("linux-x86_64"));
        assert!(e.file_for("windows-x86_64").is_none());
    }

    #[test]
    fn app_version_window() {
        let e = ExtensionIndex::parse(GOOD).unwrap().entries.remove(0);
        assert!(!e.compatible_with(Version(0, 1, 50)));
        assert!(e.compatible_with(Version(0, 1, 51)));
        assert!(e.compatible_with(Version(9, 0, 0)));
    }

    #[test]
    fn rejects_wrong_schema_and_garbage() {
        assert!(ExtensionIndex::parse("nope").is_err());
        assert!(ExtensionIndex::parse(r#"{"schema":2,"revision":1,"extensions":[]}"#).is_err());
        assert!(ExtensionIndex::parse(r#"{"schema":1,"extensions":[]}"#).is_err());
    }

    #[test]
    fn ids_are_safe_folder_names() {
        assert!(valid_id("sqlite") && valid_id("my-ext-2"));
        assert!(!valid_id("") && !valid_id("../x") && !valid_id("-x") && !valid_id("A"));
    }
}
