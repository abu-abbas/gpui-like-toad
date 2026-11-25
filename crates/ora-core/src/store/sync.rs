//! Settings Sync (stage 1): keep connection profiles, folder colors,
//! favorite queries and appearance in one small file in a folder the user
//! picks (a plain folder, or one that Google Drive / OneDrive / iCloud /
//! Dropbox already syncs), and export / import the same data as a
//! `.kilat` file.
//!
//! What is never written to the file: passwords (they stay in the OS
//! keychain), query history (only the statements the user starred), and
//! connections and tools flagged PROD unless the user approved that one.
//! The command that starts a Developer Tools server is never synced.
//!
//! Merging is three-way. `base` is what both sides looked like after the
//! last successful sync, so "changed on this computer", "changed on the
//! other computer" and "changed on both" can be told apart without
//! timestamps. Only the last case is a conflict, and it is left to the
//! user; nothing is written until every conflict has a choice.

use super::*;
use crate::credentials::{CredentialStore, ExposeSecret as _, SecretString};
use std::collections::BTreeSet;

/// Sync items by key: `conn:<id>`, `folder-color:<folder>`, `fav:<sql>`
/// and `appearance`. A missing key means "does not exist".
pub type Items = BTreeMap<String, Json>;

pub const SYNC_FILE: &str = "kilat-sync.json";
const FORMAT: &str = "kilat-sync";
const VERSION: u64 = 1;
const KEEP_VERSIONS: usize = 20;

/// What the user chose to sync (Settings > Sync > What to sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncCategories {
    pub connections: bool,
    pub favorites: bool,
    pub appearance: bool,
    /// Developer Tools entries (Redis, MailHog). Never the command that
    /// starts a server: a shared file must not decide what runs here.
    pub tools: bool,
    /// Data grid preferences (auto count).
    pub grid: bool,
    /// Saved passwords, encrypted with the sync passphrase. Off by default.
    pub passwords: bool,
    /// Which extensions this computer has, so the others can suggest them.
    pub extensions: bool,
}

impl Default for SyncCategories {
    fn default() -> Self {
        Self {
            connections: true,
            favorites: true,
            appearance: true,
            tools: true,
            grid: true,
            passwords: false,
            extensions: true,
        }
    }
}

impl SyncCategories {
    fn includes(self, key: &str) -> bool {
        match key.split_once(':').map_or(key, |(kind, _)| kind) {
            "conn" | "folder-color" => self.connections,
            "fav" => self.favorites,
            "appearance" => self.appearance,
            "tool" => self.tools,
            "grid" => self.grid,
            "secret" => self.passwords,
            "ext" => self.extensions,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Config (sync.json) and base snapshot (sync-base.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub enabled: bool,
    /// Folder that holds `kilat-sync.json` and `versions/`.
    pub folder: String,
    /// Random id of this computer, so a file can say who wrote it.
    pub device_id: String,
    pub device_name: String,
    pub auto: bool,
    pub categories: SyncCategories,
    /// PROD connections (by profile id) the user agreed to upload. A PROD
    /// connection that is not listed here never leaves this computer.
    pub prod_approved: Vec<String>,
    /// Unix seconds of the last successful sync; 0 = never.
    pub last_synced: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: String::new(),
            device_id: String::new(),
            device_name: String::new(),
            auto: true,
            categories: SyncCategories::default(),
            prod_approved: Vec::new(),
            last_synced: 0,
        }
    }
}

fn host_name() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("HOST"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "This computer".to_string())
}

pub fn load_sync_config_in(dir: &Path) -> SyncConfig {
    let mut config = SyncConfig::default();
    if let Some(v) = read_json_file(&dir.join("sync.json")) {
        let flag = |key: &str, default: bool| v.get(key).and_then(Json::as_bool).unwrap_or(default);
        let text = |key: &str| {
            v.get(key)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        config.enabled = flag("enabled", false);
        config.folder = text("folder");
        config.device_id = text("device_id");
        config.device_name = text("device_name");
        config.auto = flag("auto", true);
        config.categories = SyncCategories {
            connections: flag("connections", true),
            favorites: flag("favorites", true),
            appearance: flag("appearance", true),
            tools: flag("tools", true),
            grid: flag("grid", true),
            passwords: flag("passwords", false),
            extensions: flag("extensions", true),
        };
        config.prod_approved = v
            .get("prod_approved")
            .and_then(Json::as_arr)
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        config.last_synced = v.get("last_synced").and_then(Json::as_u64).unwrap_or(0);
    }
    if config.device_id.is_empty() {
        config.device_id = new_profile_id();
    }
    if config.device_name.trim().is_empty() {
        config.device_name = host_name();
    }
    config
}

pub fn save_sync_config_in(dir: &Path, config: &SyncConfig) -> std::io::Result<()> {
    write_json_file(
        dir,
        "sync.json",
        &Json::Obj(vec![
            ("enabled".into(), Json::Bool(config.enabled)),
            ("folder".into(), Json::Str(config.folder.clone())),
            ("device_id".into(), Json::Str(config.device_id.clone())),
            ("device_name".into(), Json::Str(config.device_name.clone())),
            ("auto".into(), Json::Bool(config.auto)),
            (
                "connections".into(),
                Json::Bool(config.categories.connections),
            ),
            ("favorites".into(), Json::Bool(config.categories.favorites)),
            (
                "appearance".into(),
                Json::Bool(config.categories.appearance),
            ),
            ("tools".into(), Json::Bool(config.categories.tools)),
            ("grid".into(), Json::Bool(config.categories.grid)),
            ("passwords".into(), Json::Bool(config.categories.passwords)),
            (
                "extensions".into(),
                Json::Bool(config.categories.extensions),
            ),
            (
                "prod_approved".into(),
                Json::Arr(
                    config
                        .prod_approved
                        .iter()
                        .cloned()
                        .map(Json::Str)
                        .collect(),
                ),
            ),
            ("last_synced".into(), Json::Num(config.last_synced as f64)),
        ]),
    )
}

pub fn load_sync_config() -> SyncConfig {
    load_sync_config_in(&config_dir())
}

pub fn save_sync_config(config: &SyncConfig) -> std::io::Result<()> {
    save_sync_config_in(&config_dir(), config)
}

fn items_to_json(items: &Items) -> Json {
    Json::Obj(items.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn items_from_json(v: Option<&Json>) -> Items {
    match v {
        Some(Json::Obj(pairs)) => pairs.iter().cloned().collect(),
        _ => Items::new(),
    }
}

fn load_base(dir: &Path) -> Items {
    items_from_json(read_json_file(&dir.join("sync-base.json")).as_ref())
}

fn save_base(dir: &Path, items: &Items) -> std::io::Result<()> {
    write_json_file(dir, "sync-base.json", &items_to_json(items))
}

// ---------------------------------------------------------------------------
// Local state <-> items
// ---------------------------------------------------------------------------

fn is_prod(p: &SavedProfile) -> bool {
    p.access.environment == ConnectionEnvironment::Production
}

fn appearance_item(dir: &Path) -> Json {
    let a = read_appearance(dir);
    let names = load_theme_names_in(dir);
    let opt = |v: Option<String>| v.map_or(Json::Null, Json::Str);
    let mut pairs = vec![
        (
            "theme".to_string(),
            Json::Str(load_theme_preference_in(dir).as_str().to_string()),
        ),
        ("light".into(), opt(names.light)),
        ("dark".into(), opt(names.dark)),
        ("accent".into(), Json::Str(a.accent)),
        ("ui_font".into(), opt(a.ui_font)),
        ("ui_font_size".into(), Json::Num(a.ui_font_size.into())),
        ("sql_font_size".into(), Json::Num(a.sql_font_size.into())),
        (
            "app_icon".into(),
            Json::Str(
                match a.app_icon {
                    AppIcon::Folded => "folded",
                    AppIcon::Bolt => "bolt",
                }
                .into(),
            ),
        ),
    ];
    if let Some(motion) = a.reduce_motion {
        pairs.push(("reduce_motion".into(), Json::Bool(motion)));
    }
    Json::Obj(pairs)
}

fn apply_appearance(dir: &Path, v: &Json) -> std::io::Result<()> {
    let str_opt = |key| match v.get(key) {
        Some(Json::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let mut a = read_appearance(dir);
    if let Some(accent) = str_opt("accent") {
        a.accent = accent;
    }
    a.ui_font = str_opt("ui_font");
    let size = |key, current: u8| {
        v.get(key)
            .and_then(Json::as_u64)
            .filter(|s| (10..=20).contains(s))
            .map_or(current, |s| s as u8)
    };
    a.ui_font_size = size("ui_font_size", a.ui_font_size);
    a.sql_font_size = size("sql_font_size", a.sql_font_size);
    a.app_icon = match v.get("app_icon").and_then(Json::as_str) {
        Some("bolt") => AppIcon::Bolt,
        _ => AppIcon::Folded,
    };
    a.reduce_motion = v.get("reduce_motion").and_then(Json::as_bool);
    write_appearance(dir, &a)?;
    if let Some(theme) = str_opt("theme") {
        save_theme_preference_in(dir, ThemePreference::from_name(&theme))?;
    }
    save_theme_names_in(
        dir,
        &ThemeNames {
            light: str_opt("light"),
            dark: str_opt("dark"),
        },
    )
}

fn grid_item(auto_count: bool) -> Json {
    Json::Obj(vec![("auto_count".into(), Json::Bool(auto_count))])
}

/// A tool as synced: without `start_server` and `server_command`.
fn tool_item(t: &ToolInstance) -> Json {
    let mut v = tool_to_json(t);
    if let Json::Obj(pairs) = &mut v {
        pairs.retain(|(k, _)| k != "start_server" && k != "server_command");
    }
    v
}

fn load_folder_colors(dir: &Path) -> BTreeMap<String, String> {
    match read_json_file(&dir.join("connection-folder-colors.json")) {
        Some(Json::Obj(entries)) => entries
            .into_iter()
            .filter_map(|(name, value)| Some((name, value.as_str()?.to_owned())))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// The local state as sync items, for the enabled categories. A PROD
/// connection is included only when its id is in `prod_ok`.
pub fn collect_items(dir: &Path, categories: SyncCategories, prod_ok: &BTreeSet<String>) -> Items {
    let mut items = Items::new();
    if categories.connections {
        for p in load_profiles_in(dir)
            .iter()
            .filter(|p| !is_prod(p) || prod_ok.contains(&p.id))
        {
            items.insert(format!("conn:{}", p.id), profile_to_json(p));
        }
        for (folder, color) in load_folder_colors(dir) {
            items.insert(format!("folder-color:{folder}"), Json::Str(color));
        }
    }
    if categories.favorites {
        for e in load_history_in(dir).iter().filter(|e| e.favorite) {
            items.insert(
                format!("fav:{}", e.sql),
                Json::Obj(vec![
                    ("sql".into(), Json::Str(e.sql.clone())),
                    ("saved_at".into(), Json::Num(e.saved_at as f64)),
                ]),
            );
        }
    }
    if categories.appearance {
        items.insert("appearance".into(), appearance_item(dir));
    }
    if categories.tools {
        for t in load_tools_in(dir).iter().filter(|t| {
            t.environment != ConnectionEnvironment::Production || prod_ok.contains(&t.id)
        }) {
            items.insert(format!("tool:{}", t.id), tool_item(t));
        }
    }
    if categories.grid {
        items.insert("grid".into(), grid_item(load_auto_count_in(dir)));
    }
    items
}

/// Make the local state match `items` for the enabled categories. Local
/// PROD connections that are not in `prod_ok` are left alone, and so are
/// history entries that are not favorites.
pub fn apply_items(
    dir: &Path,
    items: &Items,
    categories: SyncCategories,
    prod_ok: &BTreeSet<String>,
) -> std::io::Result<()> {
    if categories.connections {
        let local = load_profiles_in(dir);
        let mut wanted: BTreeMap<&str, SavedProfile> = items
            .iter()
            .filter_map(|(key, v)| {
                let id = key.strip_prefix("conn:")?;
                let mut p = profile_from_json(v)?;
                p.id = id.to_string();
                Some((id, p))
            })
            .collect();
        let mut next = Vec::new();
        for p in local {
            if is_prod(&p) && !prod_ok.contains(&p.id) {
                next.push(p);
            } else if let Some(updated) = wanted.remove(p.id.as_str()) {
                next.push(updated);
            }
            // else: deleted on the other computer.
        }
        next.extend(wanted.into_values());
        save_profiles_in(dir, &next)?;

        let colors: BTreeMap<String, String> = items
            .iter()
            .filter_map(|(key, v)| {
                Some((
                    key.strip_prefix("folder-color:")?.to_string(),
                    v.as_str()?.to_string(),
                ))
            })
            .collect();
        write_json_file(
            dir,
            "connection-folder-colors.json",
            &Json::Obj(colors.into_iter().map(|(k, v)| (k, Json::Str(v))).collect()),
        )?;
    }
    if categories.favorites {
        let mut history = load_history_in(dir);
        for e in &mut history {
            e.favorite = items.contains_key(&format!("fav:{}", e.sql));
        }
        for (key, v) in items {
            let Some(sql) = key.strip_prefix("fav:") else {
                continue;
            };
            if !history.iter().any(|e| e.sql == sql) {
                history.push(HistoryEntry {
                    sql: sql.to_string(),
                    favorite: true,
                    saved_at: v.get("saved_at").and_then(Json::as_u64).unwrap_or(0),
                });
            }
        }
        // Newest first, like the history panel. Starred ones survive the cap.
        history.sort_by_key(|e| std::cmp::Reverse(e.saved_at));
        let mut kept: Vec<HistoryEntry> = history.iter().filter(|e| e.favorite).cloned().collect();
        for e in history.into_iter().filter(|e| !e.favorite) {
            if kept.len() >= MAX_HISTORY_ENTRIES {
                break;
            }
            kept.push(e);
        }
        kept.sort_by_key(|e| std::cmp::Reverse(e.saved_at));
        save_history_in(dir, &kept)?;
    }
    if categories.appearance {
        if let Some(v) = items.get("appearance") {
            apply_appearance(dir, v)?;
        }
    }
    if categories.tools {
        let local = load_tools_in(dir);
        let mut wanted: BTreeMap<&str, ToolInstance> = items
            .iter()
            .filter_map(|(key, v)| {
                let id = key.strip_prefix("tool:")?;
                let mut t = tool_from_json(v)?;
                t.id = id.to_string();
                // Whatever a file says, it never decides what runs here.
                t.start_server = false;
                t.server_command.clear();
                Some((id, t))
            })
            .collect();
        let mut next = Vec::new();
        for t in local {
            if t.environment == ConnectionEnvironment::Production && !prod_ok.contains(&t.id) {
                next.push(t);
            } else if let Some(mut updated) = wanted.remove(t.id.as_str()) {
                // What starts a program is decided on this computer only.
                updated.start_server = t.start_server;
                updated.server_command = t.server_command;
                next.push(updated);
            }
        }
        next.extend(wanted.into_values());
        save_tools_in(dir, &next)?;
    }
    if categories.grid {
        if let Some(on) = items
            .get("grid")
            .and_then(|v| v.get("auto_count"))
            .and_then(Json::as_bool)
        {
            save_auto_count_in(dir, on)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub key: String,
    /// `None` = deleted on this computer.
    pub local: Option<Json>,
    /// `None` = deleted on the other computer.
    pub remote: Option<Json>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Local,
    Remote,
}

#[derive(Debug, Default)]
pub struct MergeResult {
    pub merged: Items,
    pub conflicts: Vec<Conflict>,
    /// Items that will change on this computer / in the sync file.
    pub pulled: usize,
    pub pushed: usize,
}

/// Three-way merge of `local` and `remote` against `base`. `choices`
/// settles conflicts; an unsettled one stays in `conflicts`.
pub fn merge(
    base: &Items,
    local: &Items,
    remote: &Items,
    choices: &BTreeMap<String, Choice>,
) -> MergeResult {
    let mut out = MergeResult::default();
    let keys: std::collections::BTreeSet<&String> = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .collect();
    for key in keys {
        let (b, l, r) = (base.get(key), local.get(key), remote.get(key));
        let winner = if l == r {
            l
        } else if l == b {
            out.pulled += 1;
            r
        } else if r == b {
            out.pushed += 1;
            l
        } else {
            match choices.get(key) {
                Some(Choice::Local) => {
                    out.pushed += 1;
                    l
                }
                Some(Choice::Remote) => {
                    out.pulled += 1;
                    r
                }
                None => {
                    out.conflicts.push(Conflict {
                        key: key.clone(),
                        local: l.cloned(),
                        remote: r.cloned(),
                    });
                    l
                }
            }
        };
        if let Some(v) = winner {
            out.merged.insert(key.clone(), v.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The sync file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFileInfo {
    pub device_name: String,
    pub saved_at: u64,
    /// Salt and check token of the sync passphrase, once one was set.
    pub crypto: Option<CryptoHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoHeader {
    pub salt: Vec<u8>,
    pub check: String,
}

impl CryptoHeader {
    fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("salt".into(), Json::Str(sync_crypto::encode(&self.salt))),
            ("check".into(), Json::Str(self.check.clone())),
        ])
    }

    fn from_json(v: &Json) -> Option<Self> {
        Some(Self {
            salt: sync_crypto::decode(v.get("salt")?.as_str()?)?,
            check: v.get("check")?.as_str()?.to_string(),
        })
    }
}

pub(super) fn sync_file_json(items: &Items, device: &str) -> Json {
    Json::Obj(vec![
        ("format".into(), Json::Str(FORMAT.into())),
        ("version".into(), Json::Num(VERSION as f64)),
        ("device".into(), Json::Str(device.into())),
        ("saved_at".into(), Json::Num(now_unix_secs() as f64)),
        ("items".into(), items_to_json(items)),
    ])
}

/// Read a sync / `.kilat` file. `Ok(None)` when it does not exist yet.
pub fn read_sync_file(path: &Path) -> Result<Option<(Items, SyncFileInfo)>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    let v = parse_json(&text)
        .map_err(|e| format!("{} is not a Kilat sync file: {e}", path.display()))?;
    if v.get("format").and_then(Json::as_str) != Some(FORMAT) {
        return Err(format!("{} is not a Kilat sync file.", path.display()));
    }
    if v.get("version").and_then(Json::as_u64).unwrap_or(0) > VERSION {
        return Err("This file was written by a newer Kilat. Update Kilat first.".into());
    }
    let info = SyncFileInfo {
        device_name: v
            .get("device")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        saved_at: v.get("saved_at").and_then(Json::as_u64).unwrap_or(0),
        crypto: v.get("crypto").and_then(CryptoHeader::from_json),
    };
    Ok(Some((items_from_json(v.get("items")), info)))
}

/// Write via a temp file in the same folder, so a cloud client never
/// uploads a half-written file.
fn write_sync_file(
    path: &Path,
    items: &Items,
    device: &str,
    crypto: Option<&CryptoHeader>,
) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let mut json = sync_file_json(items, device);
    if let (Some(header), Json::Obj(pairs)) = (crypto, &mut json) {
        pairs.push(("crypto".into(), header.to_json()));
    }
    fs::write(&tmp, to_json(&json))
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

fn backup_version(folder: &Path, file: &Path) {
    if !file.exists() {
        return;
    }
    let versions = folder.join("versions");
    if fs::create_dir_all(&versions).is_err() {
        return;
    }
    let _ = fs::copy(
        file,
        versions.join(format!("kilat-sync-{}.json", now_unix_secs())),
    );
    let mut names: Vec<_> = fs::read_dir(&versions)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    let extra = names.len().saturating_sub(KEEP_VERSIONS);
    for old in names.into_iter().take(extra) {
        let _ = fs::remove_file(old);
    }
}

/// Export the enabled categories to a `.kilat` file.
pub fn export_kilat_file(
    dir: &Path,
    path: &Path,
    categories: SyncCategories,
    prod_ok: &BTreeSet<String>,
) -> Result<usize, String> {
    let items = collect_items(dir, categories, prod_ok);
    write_sync_file(path, &items, &load_sync_config_in(dir).device_name, None)?;
    Ok(items.len())
}

// ---------------------------------------------------------------------------
// Sync and import
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum SyncOutcome {
    /// Done. Counts are items changed on this computer / in the file.
    Synced { pulled: usize, pushed: usize },
    /// Nothing was written; choose a side for each and call again.
    Conflicts(Vec<Conflict>),
}

/// What sync needs to move saved passwords: the keychain and the sync
/// passphrase. Only used when "Passwords" is on.
pub struct SyncSecrets<'a> {
    pub store: &'a dyn CredentialStore,
    pub passphrase: &'a str,
}

fn secret_id(profile_id: &str) -> String {
    format!("secret:{profile_id}")
}

/// Check `passphrase` against the sync file in `folder`. `Ok(true)`: the
/// file already has a passphrase and this is it. `Ok(false)`: nobody set
/// one yet, so this becomes the passphrase at the next sync.
pub fn check_passphrase(folder: &Path, passphrase: &str) -> Result<bool, String> {
    match read_sync_file(&folder.join(SYNC_FILE))?.and_then(|(_, info)| info.crypto) {
        Some(header) => {
            let key = sync_crypto::derive_key(passphrase, &header.salt)?;
            if sync_crypto::verify_check(&key, &header.check) {
                Ok(true)
            } else {
                Err("Wrong sync passphrase.".into())
            }
        }
        None => Ok(false),
    }
}

/// The saved passwords of this computer as sealed items. A keychain that
/// cannot be read stops the sync: reading nothing would look like "all
/// passwords were deleted here".
fn secret_items(
    dir: &Path,
    store: &dyn CredentialStore,
    key: &sync_crypto::SyncKey,
    prod_ok: &BTreeSet<String>,
) -> Result<Items, String> {
    let mut items = Items::new();
    for p in load_profiles_in(dir)
        .iter()
        .filter(|p| p.save_password && (!is_prod(p) || prod_ok.contains(&p.id)))
    {
        match store.get(&p.id) {
            Ok(Some(pw)) => {
                items.insert(
                    secret_id(&p.id),
                    Json::Str(sync_crypto::encrypt(key, &p.id, pw.expose_secret())),
                );
            }
            Ok(None) => {}
            Err(e) => return Err(format!("Cannot read saved passwords: {e}")),
        }
    }
    Ok(items)
}

/// Write the passwords that came from the file into the keychain, only
/// for profiles here that have "Save password" on.
fn apply_secrets(
    dir: &Path,
    store: &dyn CredentialStore,
    key: &sync_crypto::SyncKey,
    merged: &Items,
    local: &Items,
) -> Result<(), String> {
    let profiles = load_profiles_in(dir);
    for (k, v) in merged {
        let Some(id) = k.strip_prefix("secret:") else {
            continue;
        };
        if local.get(k) == Some(v) || !profiles.iter().any(|p| p.id == id && p.save_password) {
            continue;
        }
        let Some(pw) = v.as_str().and_then(|t| sync_crypto::decrypt(key, id, t)) else {
            continue;
        };
        store
            .set(id, &SecretString::from(pw))
            .map_err(|e| format!("Cannot save a synced password: {e}"))?;
    }
    Ok(())
}

fn extension_ids(dir: &Path) -> Json {
    let ext = load_extensions_in(dir);
    Json::Arr(
        REGISTRY
            .iter()
            .filter(|e| !e.built_in && ext.is_installed(e.id))
            .map(|e| Json::Str(e.id.to_string()))
            .collect(),
    )
}

/// Extensions that other computers have and this one has not, as
/// (computer name, extensions). Only a suggestion: nothing is installed.
pub fn extension_suggestions(dir: &Path) -> Vec<(String, Vec<&'static ExtensionInfo>)> {
    let own = format!("ext:{}", load_sync_config_in(dir).device_id);
    let ext = load_extensions_in(dir);
    load_base(dir)
        .iter()
        .filter(|(k, _)| k.starts_with("ext:") && **k != own)
        .filter_map(|(_, v)| {
            let name = v.get("device").and_then(Json::as_str)?.to_string();
            let missing: Vec<&'static ExtensionInfo> = v
                .get("ids")
                .and_then(Json::as_arr)?
                .iter()
                .filter_map(|id| extension_info(id.as_str()?))
                .filter(|info| !info.built_in && !ext.is_installed(info.id))
                .collect();
            (!missing.is_empty()).then_some((name, missing))
        })
        .collect()
}

/// Sync with the configured folder. Pass the user's `choices` for the
/// conflicts of an earlier call that returned [`SyncOutcome::Conflicts`].
/// `secrets` is needed only when passwords are synced.
pub fn sync_with(
    dir: &Path,
    choices: &BTreeMap<String, Choice>,
    secrets: Option<&SyncSecrets>,
) -> Result<SyncOutcome, String> {
    let mut config = load_sync_config_in(dir);
    if !config.enabled || config.folder.trim().is_empty() {
        return Err("Sync is not set up.".into());
    }
    let folder = PathBuf::from(&config.folder);
    if !folder.is_dir() {
        return Err(format!(
            "The sync folder {} was not found.",
            folder.display()
        ));
    }
    let file = folder.join(SYNC_FILE);
    let cats = config.categories;
    let base = load_base(dir);
    let remote_file = read_sync_file(&file)?;
    let mut header = remote_file.as_ref().and_then(|(_, i)| i.crypto.clone());

    // The key for passwords: the file's salt if there is one, else a new
    // salt that is written with the next upload.
    let key = if cats.passwords {
        let secrets = secrets.ok_or("The sync passphrase is not set on this computer.")?;
        match &header {
            Some(h) => {
                let key = sync_crypto::derive_key(secrets.passphrase, &h.salt)?;
                if !sync_crypto::verify_check(&key, &h.check) {
                    return Err("Wrong sync passphrase.".into());
                }
                Some(key)
            }
            None => {
                let salt = sync_crypto::new_salt();
                let key = sync_crypto::derive_key(secrets.passphrase, &salt)?;
                header = Some(CryptoHeader {
                    salt: salt.to_vec(),
                    check: sync_crypto::check_token(&key),
                });
                Some(key)
            }
        }
    } else {
        None
    };

    // A PROD connection already in the file was approved when it was
    // uploaded, so it stays in sync even if this list is empty here.
    let prod_ok: BTreeSet<String> = config
        .prod_approved
        .iter()
        .cloned()
        .chain(base.keys().filter_map(|k| {
            k.strip_prefix("conn:")
                .or_else(|| k.strip_prefix("tool:"))
                .map(str::to_string)
        }))
        .collect();
    let mut local = collect_items(dir, cats, &prod_ok);
    if let (Some(secrets), Some(key)) = (secrets, &key) {
        local.extend(secret_items(dir, secrets.store, key, &prod_ok)?);
    }
    if cats.extensions {
        let own = format!("ext:{}", config.device_id);
        local.insert(
            own.clone(),
            Json::Obj(vec![
                ("device".into(), Json::Str(config.device_name.clone())),
                ("ids".into(), extension_ids(dir)),
            ]),
        );
        // Other computers' entries are theirs: carry them over untouched,
        // or they would look deleted here.
        for (k, v) in base
            .iter()
            .filter(|(k, _)| k.starts_with("ext:") && **k != own)
        {
            local.insert(k.clone(), v.clone());
        }
    }
    // A computer that never touched its appearance must not fight the
    // synced one over it on the first sync: let the file win.
    if base.is_empty() {
        let pristine = [
            ("appearance", appearance_item(&dir.join("__none__"))),
            ("grid", grid_item(false)),
        ];
        for (key, default) in pristine {
            if local.get(key) == Some(&default) {
                local.remove(key);
            }
        }
    }
    let remote: Items = remote_file
        .as_ref()
        .map(|(items, _)| {
            items
                .iter()
                .filter(|(k, _)| cats.includes(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut result = merge(&base, &local, &remote, choices);
    if !result.conflicts.is_empty() {
        return Ok(SyncOutcome::Conflicts(result.conflicts));
    }
    // A password missing here (cleared keychain) must not delete it for
    // every other computer.
    for (k, v) in remote.iter().filter(|(k, _)| k.starts_with("secret:")) {
        result.merged.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let merged = result.merged;
    if merged != local {
        apply_items(dir, &merged, cats, &prod_ok)
            .map_err(|e| format!("Cannot update this computer: {e}"))?;
        if let (Some(secrets), Some(key)) = (secrets, &key) {
            apply_secrets(dir, secrets.store, key, &merged, &local)?;
        }
    }
    let new_header = remote_file.as_ref().and_then(|(_, i)| i.crypto.as_ref()) != header.as_ref();
    if merged != remote || !file.exists() || new_header {
        // Keep other computers' items of categories this computer does not sync.
        let mut out = merged.clone();
        if let Some((all, _)) = &remote_file {
            for (k, v) in all {
                if !cats.includes(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        backup_version(&folder, &file);
        write_sync_file(&file, &out, &config.device_name, header.as_ref())?;
    }
    save_base(dir, &merged).map_err(|e| e.to_string())?;
    config.last_synced = now_unix_secs();
    save_sync_config_in(dir, &config).map_err(|e| e.to_string())?;
    Ok(SyncOutcome::Synced {
        pulled: result.pulled,
        pushed: result.pushed,
    })
}

/// Import a `.kilat` file: everything new is added, and anything that
/// differs from what is here is a conflict for the user to settle. Never
/// deletes, and never touches the sync file or the sync base.
pub fn import_kilat_file(
    dir: &Path,
    path: &Path,
    categories: SyncCategories,
    choices: &BTreeMap<String, Choice>,
) -> Result<SyncOutcome, String> {
    let prod_ok = BTreeSet::new();
    let (remote, _) =
        read_sync_file(path)?.ok_or_else(|| format!("{} was not found.", path.display()))?;
    let remote: Items = remote
        .into_iter()
        .filter(|(k, _)| categories.includes(k) && !k.starts_with("ext:"))
        .collect();
    let local = collect_items(dir, categories, &prod_ok);
    // Empty base: only-in-the-file is an addition, only-here is kept, so an
    // import never deletes anything.
    let result = merge(&Items::new(), &local, &remote, choices);
    if !result.conflicts.is_empty() {
        return Ok(SyncOutcome::Conflicts(result.conflicts));
    }
    let pulled = result
        .merged
        .iter()
        .filter(|(k, v)| local.get(*k) != Some(*v))
        .count();
    apply_items(dir, &result.merged, categories, &prod_ok)
        .map_err(|e| format!("Cannot import: {e}"))?;
    Ok(SyncOutcome::Synced { pulled, pushed: 0 })
}

/// Turn sync off. The folder and its file stay as they are.
pub fn disable_sync_in(dir: &Path) -> std::io::Result<()> {
    let mut config = load_sync_config_in(dir);
    config.enabled = false;
    save_sync_config_in(dir, &config)?;
    let _ = fs::remove_file(dir.join("sync-base.json"));
    Ok(())
}

/// Cloud-synced folders found on this computer (Google Drive for desktop,
/// OneDrive, iCloud Drive, Dropbox), as (label, folder to use). The folder
/// is a `Kilat` subfolder; it is created when the user picks it.
pub fn detect_cloud_folders() -> Vec<(String, PathBuf)> {
    let home = home_dir();
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut add = |label: &str, root: PathBuf| {
        if root.is_dir() && !found.iter().any(|(_, p)| p.starts_with(&root)) {
            found.push((label.to_string(), root.join("Kilat")));
        }
    };
    add("Google Drive", home.join("Google Drive").join("My Drive"));
    add("Google Drive", home.join("My Drive"));
    add(
        "iCloud Drive",
        home.join("Library/Mobile Documents/com~apple~CloudDocs"),
    );
    add("OneDrive", home.join("OneDrive"));
    add("Dropbox", home.join("Dropbox"));
    if let Ok(entries) = fs::read_dir(home.join("Library/CloudStorage")) {
        let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        for path in names {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if name.starts_with("GoogleDrive-") {
                add("Google Drive", path.join("My Drive"));
            } else if name.starts_with("OneDrive") {
                add("OneDrive", path.clone());
            } else if name.starts_with("Dropbox") {
                add("Dropbox", path.clone());
            }
        }
    }
    found
}
