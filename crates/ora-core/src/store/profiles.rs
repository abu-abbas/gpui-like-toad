//! Saved connection profiles and sidebar folders.

use super::*;

/// A saved connection profile. No password field by design: when
/// `save_password` is set, the password is in the OS keychain keyed by
/// `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    /// Stable id, the keychain account name for this profile's password.
    /// Unlike the name it never changes when the profile is edited.
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    pub username: String,
    pub connect_string: String,
    /// Optional folder in the Connections tree.
    pub folder: String,
    pub access: ConnectionAccess,
    /// The user ticked "Save password": connect without asking.
    pub save_password: bool,
}

/// A new, unique profile id: wall-clock nanoseconds, process id, and a
/// per-process counter, in hex. Not a secret, only needs to be unique
/// within one user's profile list.
pub fn new_profile_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("p{nanos:x}-{:x}-{n:x}", std::process::id())
}

// ---------------------------------------------------------------------------
// Saved profiles
// ---------------------------------------------------------------------------

pub(super) fn profile_to_json(p: &SavedProfile) -> Json {
    Json::Obj(vec![
        ("id".to_string(), Json::Str(p.id.clone())),
        ("name".to_string(), Json::Str(p.name.clone())),
        ("kind".to_string(), Json::Str(p.kind.key().to_string())),
        ("username".to_string(), Json::Str(p.username.clone())),
        (
            "connect_string".to_string(),
            Json::Str(p.connect_string.clone()),
        ),
        ("folder".to_string(), Json::Str(p.folder.clone())),
        (
            "environment".into(),
            Json::Str(p.access.environment.label().into()),
        ),
        ("read_only".into(), Json::Bool(p.access.read_only)),
        (
            "protect_production".into(),
            Json::Bool(p.access.protect_production),
        ),
        ("sysdba".into(), Json::Bool(p.access.sysdba)),
        ("record_history".into(), Json::Bool(!p.access.skip_history)),
        (
            "trust_server_certificate".into(),
            Json::Bool(p.access.trust_server_certificate),
        ),
        (
            "allow_legacy_tls".into(),
            Json::Bool(p.access.allow_legacy_tls),
        ),
        ("save_password".to_string(), Json::Bool(p.save_password)),
    ])
}

/// Parse one profile. Files from before ids existed have no `id`; those
/// get `None` here and a fresh id from the loader.
pub(super) fn profile_from_json(v: &Json) -> Option<SavedProfile> {
    Some(SavedProfile {
        id: v
            .get("id")
            .and_then(Json::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_default(),
        name: v.get("name")?.as_str()?.to_string(),
        kind: DbKind::from_key(v.get("kind").and_then(Json::as_str).unwrap_or_default()),
        username: v.get("username")?.as_str()?.to_string(),
        connect_string: v.get("connect_string")?.as_str()?.to_string(),
        folder: v
            .get("folder")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        access: ConnectionAccess {
            environment: ConnectionEnvironment::parse(
                v.get("environment")
                    .and_then(Json::as_str)
                    .unwrap_or_default(),
            ),
            read_only: v.get("read_only").and_then(Json::as_bool).unwrap_or(false),
            protect_production: v
                .get("protect_production")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            sysdba: v.get("sysdba").and_then(Json::as_bool).unwrap_or(false),
            trust_server_certificate: v
                .get("trust_server_certificate")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            allow_legacy_tls: v
                .get("allow_legacy_tls")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            // Files from before this option keep recording history.
            skip_history: !v
                .get("record_history")
                .and_then(Json::as_bool)
                .unwrap_or(true),
        },
        save_password: v
            .get("save_password")
            .and_then(Json::as_bool)
            .unwrap_or(false),
    })
}

/// Load saved profiles. Profiles without an id (older files) or with a
/// duplicate id get a new one, and the file is rewritten best-effort so
/// the id stays stable: it is the keychain key for a saved password.
pub fn load_profiles_in(dir: &Path) -> Vec<SavedProfile> {
    let mut profiles: Vec<SavedProfile> = match read_json_file(&dir.join("connections.json")) {
        Some(Json::Arr(items)) => items.iter().filter_map(profile_from_json).collect(),
        _ => Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut changed = false;
    for p in &mut profiles {
        if p.id.is_empty() || !seen.insert(p.id.clone()) {
            p.id = new_profile_id();
            seen.insert(p.id.clone());
            changed = true;
        }
    }
    if changed {
        let _ = save_profiles_in(dir, &profiles);
    }
    profiles
}

pub fn save_profiles_in(dir: &Path, profiles: &[SavedProfile]) -> std::io::Result<()> {
    write_json_file(
        dir,
        "connections.json",
        &Json::Arr(profiles.iter().map(profile_to_json).collect()),
    )
}

pub fn load_profiles() -> Vec<SavedProfile> {
    load_profiles_in(&config_dir())
}

pub fn save_profiles(profiles: &[SavedProfile]) -> std::io::Result<()> {
    save_profiles_in(&config_dir(), profiles)
}

/// Whether `folder` is `root` or lies somewhere below it.
pub fn folder_in(folder: &str, root: &str) -> bool {
    folder == root
        || folder
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Re-root `folder` from `from` to `to` (`to` empty means the top level):
/// `a/b/c` moved from `a/b` to `x` becomes `x/c`, and `a/b` itself becomes
/// `x`. `None` when `folder` is not inside `from`.
pub fn folder_rebase(folder: &str, from: &str, to: &str) -> Option<String> {
    if !folder_in(folder, from) {
        return None;
    }
    let rest = folder[from.len()..].trim_start_matches('/');
    Some(match (to.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_owned(),
        (false, true) => to.to_owned(),
        (false, false) => format!("{to}/{rest}"),
    })
}

/// Local colors for sidebar folders. A color can outlive a folder becoming
/// empty so it is restored when a connection is moved back into that folder.
pub fn load_connection_folder_colors() -> BTreeMap<String, String> {
    match read_json_file(&config_dir().join("connection-folder-colors.json")) {
        Some(Json::Obj(entries)) => entries
            .into_iter()
            .filter_map(|(name, value)| Some((name, value.as_str()?.to_owned())))
            .collect(),
        _ => BTreeMap::new(),
    }
}

pub fn save_connection_folder_colors(colors: &BTreeMap<String, String>) -> std::io::Result<()> {
    write_json_file(
        &config_dir(),
        "connection-folder-colors.json",
        &Json::Obj(
            colors
                .iter()
                .map(|(folder, color)| (folder.clone(), Json::Str(color.clone())))
                .collect(),
        ),
    )
}
