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

use crate::{ConnectionAccess, ConnectionEnvironment};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A saved connection profile. No password field by design: when
/// `save_password` is set, the password is in the OS keychain keyed by
/// `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    /// Stable id, the keychain account name for this profile's password.
    /// Unlike the name it never changes when the profile is edited.
    pub id: String,
    pub name: String,
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

/// One executed statement in the history list. Newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub sql: String,
    pub favorite: bool,
    /// Unix seconds when the statement was recorded.
    pub saved_at: u64,
}

/// Maximum history entries kept on disk (and in the panel).
pub const MAX_HISTORY_ENTRIES: usize = 50;

/// Theme preference from the Settings panel. `System` follows the OS
/// appearance; `Light`/`Dark` pin one palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "light" => ThemePreference::Light,
            "dark" => ThemePreference::Dark,
            _ => ThemePreference::System,
        }
    }
}

/// Seconds since the Unix epoch, for [`HistoryEntry::saved_at`].
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Prepend `sql` to the history, dropping any older identical entry and
/// capping the list at [`MAX_HISTORY_ENTRIES`]. A re-run statement keeps
/// its favorite flag when it was already in the list. Empty statements
/// are ignored.
pub fn record_history(entries: &mut Vec<HistoryEntry>, sql: &str, now: u64) {
    let sql = sql.trim();
    if sql.is_empty() {
        return;
    }
    let favorite = entries
        .iter()
        .find(|e| e.sql == sql)
        .map(|e| e.favorite)
        .unwrap_or(false);
    entries.retain(|e| e.sql != sql);
    entries.insert(
        0,
        HistoryEntry {
            sql: sql.to_string(),
            favorite,
            saved_at: now,
        },
    );
    entries.truncate(MAX_HISTORY_ENTRIES);
}

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
// Minimal JSON
// ---------------------------------------------------------------------------

/// Minimal JSON value: enough for the small settings files this module
/// owns. Objects preserve insertion order (a `Vec` of pairs) so files
/// stay human-readable and stable across saves.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        }
    }
}

/// Parse a JSON document. Returns a human-readable error on malformed
/// input; the store's loaders treat that as "use the default".
pub fn parse_json(input: &str) -> Result<Json, String> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.parse_value(0)?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(format!("trailing characters at offset {}", p.pos));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.next() == Some(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' at offset {}", self.pos))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Json, String> {
        if depth >= 64 {
            return Err("JSON is nested too deeply".into());
        }
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(depth + 1),
            Some('[') => self.parse_array(depth + 1),
            Some('"') => Ok(Json::Str(self.parse_string()?)),
            Some('t') => self.parse_literal("true", Json::Bool(true)),
            Some('f') => self.parse_literal("false", Json::Bool(false)),
            Some('n') => self.parse_literal("null", Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{c}' at offset {}", self.pos)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn parse_literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for expected in word.chars() {
            if self.next() != Some(expected) {
                return Err(format!("invalid literal at offset {}", self.pos));
            }
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number '{text}' at offset {start}"))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next() {
                None => return Err("unterminated string".to_string()),
                Some('"') => return Ok(out),
                Some('\\') => {
                    let esc = self.next().ok_or("unterminated escape")?;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000C}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // High surrogate: expect a \uXXXX low
                                // surrogate right after.
                                if self.next() == Some('\\') && self.next() == Some('u') {
                                    let lo = self.parse_hex4()?;
                                    let combined =
                                        0x10000 + ((cp - 0xD800) << 10) + lo.wrapping_sub(0xDC00);
                                    out.push(
                                        char::from_u32(combined)
                                            .unwrap_or(char::REPLACEMENT_CHARACTER),
                                    );
                                } else {
                                    return Err(format!(
                                        "unpaired surrogate at offset {}",
                                        self.pos
                                    ));
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
                            }
                        }
                        other => {
                            return Err(format!(
                                "invalid escape '\\{other}' at offset {}",
                                self.pos
                            ))
                        }
                    }
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let c = self.next().ok_or("truncated \\u escape")?;
            let digit = c.to_digit(16).ok_or("invalid \\u escape")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.next() {
                Some(',') => {}
                Some(']') => return Ok(Json::Arr(items)),
                _ => return Err(format!("expected ',' or ']' at offset {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Json, String> {
        self.expect('{')?;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(format!("expected object key at offset {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value(depth)?;
            pairs.push((key, value));
            self.skip_ws();
            match self.next() {
                Some(',') => {}
                Some('}') => return Ok(Json::Obj(pairs)),
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.pos)),
            }
        }
    }
}

/// Serialize a value as compact JSON. Strings are escaped per RFC 8259;
/// non-ASCII characters pass through as UTF-8.
pub fn to_json(value: &Json) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_json(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9_007_199_254_740_992.0 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Json::Str(s) => write_json_string(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
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

// ---------------------------------------------------------------------------
// Saved profiles
// ---------------------------------------------------------------------------

fn profile_to_json(p: &SavedProfile) -> Json {
    Json::Obj(vec![
        ("id".to_string(), Json::Str(p.id.clone())),
        ("name".to_string(), Json::Str(p.name.clone())),
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
        ("save_password".to_string(), Json::Bool(p.save_password)),
    ])
}

/// Parse one profile. Files from before ids existed have no `id`; those
/// get `None` here and a fresh id from the loader.
fn profile_from_json(v: &Json) -> Option<SavedProfile> {
    Some(SavedProfile {
        id: v
            .get("id")
            .and_then(Json::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_default(),
        name: v.get("name")?.as_str()?.to_string(),
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

/// A portable list of connection settings. Keychain IDs and the saved
/// password flag stay machine-local.
pub fn export_connection_profiles(profiles: &[SavedProfile]) -> String {
    let items = profiles
        .iter()
        .map(profile_to_json)
        .map(|json| {
            let Json::Obj(mut fields) = json else {
                unreachable!("profile is an object");
            };
            fields.retain(|(key, _)| !matches!(key.as_str(), "id" | "save_password"));
            Json::Obj(fields)
        })
        .collect();
    to_json(&Json::Obj(vec![
        ("format".into(), Json::Str("ora-connections-v1".into())),
        ("connections".into(), Json::Arr(items)),
    ]))
}

/// Read Ora's portable format or its older bare connection list. A
/// transferred profile never points to another machine's keychain and
/// must explicitly be opted into SYSDBA after import.
pub fn import_connection_profiles(text: &str) -> Result<Vec<SavedProfile>, String> {
    let value = parse_json(text)?;
    let items = match &value {
        Json::Arr(items) => items.as_slice(),
        _ if value.get("format").and_then(Json::as_str) == Some("ora-connections-v1") => value
            .get("connections")
            .and_then(Json::as_arr)
            .ok_or("Connections must be a JSON array")?,
        _ => {
            return Err(
                "Unsupported connection file. Expected an Ora connections JSON file.".into(),
            )
        }
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut profile = profile_from_json(item)
                .filter(|p| {
                    !p.name.trim().is_empty()
                        && !p.username.trim().is_empty()
                        && !p.connect_string.trim().is_empty()
                })
                .ok_or_else(|| {
                    format!(
                        "Connection {} needs a name, username, and connect string",
                        index + 1
                    )
                })?;
            profile.id = new_profile_id();
            profile.save_password = false;
            profile.access.sysdba = false;
            Ok(profile)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn history_to_json(e: &HistoryEntry) -> Json {
    Json::Obj(vec![
        ("sql".to_string(), Json::Str(e.sql.clone())),
        ("favorite".to_string(), Json::Bool(e.favorite)),
        ("saved_at".to_string(), Json::Num(e.saved_at as f64)),
    ])
}

fn history_from_json(v: &Json) -> Option<HistoryEntry> {
    Some(HistoryEntry {
        sql: v.get("sql")?.as_str()?.to_string(),
        favorite: v.get("favorite").and_then(Json::as_bool).unwrap_or(false),
        saved_at: v.get("saved_at").and_then(Json::as_u64).unwrap_or(0),
    })
}

pub fn load_history_in(dir: &Path) -> Vec<HistoryEntry> {
    match read_json_file(&dir.join("history.json")) {
        Some(Json::Arr(items)) => {
            let mut entries: Vec<HistoryEntry> =
                items.iter().filter_map(history_from_json).collect();
            entries.truncate(MAX_HISTORY_ENTRIES);
            entries
        }
        _ => Vec::new(),
    }
}

pub fn save_history_in(dir: &Path, entries: &[HistoryEntry]) -> std::io::Result<()> {
    write_json_file(
        dir,
        "history.json",
        &Json::Arr(
            entries
                .iter()
                .take(MAX_HISTORY_ENTRIES)
                .map(history_to_json)
                .collect(),
        ),
    )
}

pub fn load_history() -> Vec<HistoryEntry> {
    load_history_in(&config_dir())
}

pub fn save_history(entries: &[HistoryEntry]) -> std::io::Result<()> {
    save_history_in(&config_dir(), entries)
}

// ---------------------------------------------------------------------------
// Theme preference
// ---------------------------------------------------------------------------

pub fn load_theme_preference_in(dir: &Path) -> ThemePreference {
    match read_json_file(&dir.join("settings.json")) {
        Some(v) => v
            .get("theme")
            .and_then(Json::as_str)
            .map(ThemePreference::from_name)
            .unwrap_or_default(),
        None => ThemePreference::default(),
    }
}

pub fn save_theme_preference_in(dir: &Path, pref: ThemePreference) -> std::io::Result<()> {
    // Preserve any other settings keys already in the file.
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert("theme".to_string(), Json::Str(pref.as_str().to_string()));
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

/// Which panels of the work area the user minimized, remembered across
/// restarts: the Info panel under a data tab's grid and the Explain Plan
/// panel beside a SQL tab's results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelLayout {
    pub info_collapsed: bool,
    pub plan_collapsed: bool,
}

const INFO_COLLAPSED_KEY: &str = "info_panel_collapsed";
const PLAN_COLLAPSED_KEY: &str = "plan_panel_collapsed";

pub fn load_panel_layout_in(dir: &Path) -> PanelLayout {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return PanelLayout::default();
    };
    let flag = |key: &str| v.get(key).and_then(Json::as_bool).unwrap_or(false);
    PanelLayout {
        info_collapsed: flag(INFO_COLLAPSED_KEY),
        plan_collapsed: flag(PLAN_COLLAPSED_KEY),
    }
}

pub fn save_panel_layout_in(dir: &Path, layout: PanelLayout) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert(INFO_COLLAPSED_KEY.into(), Json::Bool(layout.info_collapsed));
    pairs.insert(PLAN_COLLAPSED_KEY.into(), Json::Bool(layout.plan_collapsed));
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_panel_layout() -> PanelLayout {
    load_panel_layout_in(&config_dir())
}

pub fn save_panel_layout(layout: PanelLayout) -> std::io::Result<()> {
    save_panel_layout_in(&config_dir(), layout)
}

/// Names of the color themes picked for light and dark mode. `None` means
/// the app's own palette ("Ora"). Names match gpui-kit `ThemeRegistry`
/// entries, e.g. "Ayu Light" or "Tokyo Night".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeNames {
    pub light: Option<String>,
    pub dark: Option<String>,
}

pub fn load_theme_names_in(dir: &Path) -> ThemeNames {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return ThemeNames::default();
    };
    let name = |key: &str| {
        v.get(key)
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    ThemeNames {
        light: name("light_theme"),
        dark: name("dark_theme"),
    }
}

pub fn save_theme_names_in(dir: &Path, names: &ThemeNames) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    for (key, value) in [("light_theme", &names.light), ("dark_theme", &names.dark)] {
        match value {
            Some(name) => {
                pairs.insert(key.to_string(), Json::Str(name.clone()));
            }
            None => {
                pairs.remove(key);
            }
        }
    }
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_theme_names() -> ThemeNames {
    load_theme_names_in(&config_dir())
}

pub fn save_theme_names(names: &ThemeNames) -> std::io::Result<()> {
    save_theme_names_in(&config_dir(), names)
}

pub fn load_theme_preference() -> ThemePreference {
    load_theme_preference_in(&config_dir())
}

pub fn save_theme_preference(pref: ThemePreference) -> std::io::Result<()> {
    save_theme_preference_in(&config_dir(), pref)
}

// ---------------------------------------------------------------------------
// Update-check preferences
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearancePrefs {
    pub accent: String,
    pub ui_font: Option<String>,
    pub ui_font_size: u8,
    pub sql_font_size: u8,
    /// None follows the platform default.
    pub reduce_motion: Option<bool>,
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            accent: "Violet".into(),
            ui_font: None,
            ui_font_size: 13,
            sql_font_size: 13,
            reduce_motion: None,
        }
    }
}

fn read_appearance(dir: &Path) -> AppearancePrefs {
    let default = AppearancePrefs::default();
    let Some(settings) = read_json_file(&dir.join("settings.json")) else {
        return default;
    };
    let size = |key, default| {
        settings
            .get(key)
            .and_then(Json::as_u64)
            .filter(|size| (10..=20).contains(size))
            .map_or(default, |size| size as u8)
    };
    AppearancePrefs {
        accent: settings
            .get("accent")
            .and_then(Json::as_str)
            .map_or(default.accent, str::to_owned),
        ui_font: settings
            .get("ui_font")
            .and_then(Json::as_str)
            .map(str::to_owned),
        ui_font_size: size("ui_font_size", default.ui_font_size),
        sql_font_size: size("sql_font_size", default.sql_font_size),
        reduce_motion: settings.get("reduce_motion").and_then(Json::as_bool),
    }
}

pub fn load_appearance() -> AppearancePrefs {
    read_appearance(&config_dir())
}

fn write_appearance(dir: &Path, prefs: &AppearancePrefs) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    if let Some(font) = &prefs.ui_font {
        pairs.insert("ui_font".into(), Json::Str(font.clone()));
    } else {
        pairs.remove("ui_font");
    }
    pairs.insert("accent".into(), Json::Str(prefs.accent.clone()));
    pairs.insert("ui_font_size".into(), Json::Num(prefs.ui_font_size.into()));
    pairs.insert(
        "sql_font_size".into(),
        Json::Num(prefs.sql_font_size.into()),
    );
    if let Some(reduce_motion) = prefs.reduce_motion {
        pairs.insert("reduce_motion".into(), Json::Bool(reduce_motion));
    } else {
        pairs.remove("reduce_motion");
    }
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn save_appearance(prefs: &AppearancePrefs) -> std::io::Result<()> {
    write_appearance(&config_dir(), prefs)
}

/// Settings for the GitHub Releases update check, kept in `settings.json`
/// next to the theme. Nothing here identifies the user or the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePrefs {
    /// Check automatically at startup (at most once per interval).
    pub auto_check: bool,
    /// Unix seconds of the last request to GitHub (0 = never).
    pub last_checked: u64,
    /// Version the user dismissed the banner for; not announced again.
    pub dismissed_version: Option<String>,
    /// Most recent app version launched on this machine.
    pub last_launched_version: Option<String>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_checked: 0,
            dismissed_version: None,
            last_launched_version: None,
        }
    }
}

pub fn load_update_prefs_in(dir: &Path) -> UpdatePrefs {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return UpdatePrefs::default();
    };
    UpdatePrefs {
        auto_check: v
            .get("update_auto_check")
            .and_then(Json::as_bool)
            .unwrap_or(true),
        last_checked: v
            .get("update_last_checked")
            .and_then(Json::as_u64)
            .unwrap_or(0),
        dismissed_version: v
            .get("update_dismissed_version")
            .and_then(Json::as_str)
            .map(str::to_string),
        last_launched_version: v
            .get("update_last_launched_version")
            .and_then(Json::as_str)
            .map(str::to_string),
    }
}

pub fn save_update_prefs_in(dir: &Path, prefs: &UpdatePrefs) -> std::io::Result<()> {
    // Preserve any other settings keys already in the file.
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert("update_auto_check".into(), Json::Bool(prefs.auto_check));
    pairs.insert(
        "update_last_checked".into(),
        Json::Num(prefs.last_checked as f64),
    );
    match &prefs.dismissed_version {
        Some(v) => pairs.insert("update_dismissed_version".into(), Json::Str(v.clone())),
        None => pairs.remove("update_dismissed_version"),
    };
    match &prefs.last_launched_version {
        Some(v) => pairs.insert("update_last_launched_version".into(), Json::Str(v.clone())),
        None => pairs.remove("update_last_launched_version"),
    };
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_update_prefs() -> UpdatePrefs {
    load_update_prefs_in(&config_dir())
}

pub fn save_update_prefs(prefs: &UpdatePrefs) -> std::io::Result<()> {
    save_update_prefs_in(&config_dir(), prefs)
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_paths_rebase() {
        use super::{folder_in, folder_rebase};
        assert!(folder_in("a/b", "a") && !folder_in("ab", "a") && folder_in("a", "a"));
        assert_eq!(folder_rebase("a/b/c", "a/b", "x").as_deref(), Some("x/c"));
        assert_eq!(folder_rebase("a/b", "a/b", "x").as_deref(), Some("x"));
        assert_eq!(folder_rebase("a/b", "a/b", "").as_deref(), Some(""));
        assert_eq!(folder_rebase("a/b/c", "a/b", "").as_deref(), Some("c"));
        assert_eq!(folder_rebase("ab", "a", "x"), None);
    }

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        // Test-only scratch dir, unique per process id and test name.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = env::temp_dir().join(format!(
            "gpui-like-toad-store-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn import_preserves_environment_without_reusing_credentials() {
        let original = SavedProfile {
            id: "machine-keychain-id".into(),
            name: "Production".into(),
            username: "REPORTS".into(),
            connect_string: "host:1521/prod".into(),
            folder: "Oracle".into(),
            access: ConnectionAccess {
                read_only: true,
                protect_production: true,
                sysdba: true,
                environment: ConnectionEnvironment::Production,
            },
            save_password: true,
        };
        let portable = export_connection_profiles(std::slice::from_ref(&original));
        assert!(!portable.contains(&original.id));
        assert!(!portable.contains("save_password"));
        let imported = &import_connection_profiles(&portable).unwrap()[0];
        assert_ne!(imported.id, original.id);
        assert_eq!(imported.folder, original.folder);
        assert!(imported.access.read_only);
        assert!(imported.access.protect_production);
        assert_eq!(
            imported.access.environment,
            ConnectionEnvironment::Production
        );
        assert!(!imported.access.sysdba);
        assert!(!imported.save_password);
        assert!(import_connection_profiles(&"[".repeat(80)).is_err());
    }

    #[test]
    fn appearance_round_trips_alongside_theme_preference() {
        let dir = scratch("appearance");
        let prefs = AppearancePrefs {
            accent: "Teal".into(),
            ui_font: Some("Arial".into()),
            ui_font_size: 15,
            sql_font_size: 17,
            reduce_motion: Some(true),
        };
        assert_eq!(read_appearance(&dir), AppearancePrefs::default());
        save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
        write_appearance(&dir, &prefs).unwrap();
        assert_eq!(read_appearance(&dir), prefs);
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn json_round_trip() {
        let value = Json::Obj(vec![
            ("name".to_string(), Json::Str("HR \"prod\"".to_string())),
            ("n".to_string(), Json::Num(42.0)),
            ("f".to_string(), Json::Num(1.5)),
            ("ok".to_string(), Json::Bool(true)),
            ("nothing".to_string(), Json::Null),
            (
                "list".to_string(),
                Json::Arr(vec![
                    Json::Str("a\nb\t\\".to_string()),
                    Json::Str("é😀".to_string()),
                    Json::Num(-3.0),
                ]),
            ),
        ]);
        let text = to_json(&value);
        assert_eq!(parse_json(&text).unwrap(), value);
        assert!(text.contains("\\n"));
        assert!(text.contains("é😀"));
    }

    #[test]
    fn json_rejects_malformed_input() {
        assert!(parse_json("{").is_err());
        assert!(parse_json("[1,]").is_err());
        assert!(parse_json("{\"a\": }").is_err());
        assert!(parse_json("\"unterminated").is_err());
        assert!(parse_json("tru").is_err());
        assert!(parse_json("{} extra").is_err());
        assert!(parse_json("").is_err());
        assert_eq!(
            parse_json("  { \"a\" : [1, 2.5, true, null] }  ").unwrap(),
            Json::Obj(vec![(
                "a".to_string(),
                Json::Arr(vec![
                    Json::Num(1.0),
                    Json::Num(2.5),
                    Json::Bool(true),
                    Json::Null
                ])
            )])
        );
    }

    #[test]
    fn json_unicode_escapes() {
        assert_eq!(
            parse_json("\"\\u0041\\u00e9\"").unwrap(),
            Json::Str("Aé".to_string())
        );
        // Surrogate pair for U+1F600.
        assert_eq!(
            parse_json("\"\\uD83D\\uDE00\"").unwrap(),
            Json::Str("😀".to_string())
        );
    }

    #[test]
    fn profiles_round_trip_and_fail_soft() {
        let dir = scratch("profiles");
        assert!(load_profiles_in(&dir).is_empty());
        let profiles = vec![
            SavedProfile {
                id: "p1".to_string(),
                name: "HR Local".to_string(),
                username: "hr".to_string(),
                connect_string: "localhost:1521/FREEPDB1".to_string(),
                folder: "Work".into(),
                access: Default::default(),
                save_password: true,
            },
            SavedProfile {
                id: "p2".to_string(),
                name: "Prod \"A\"".to_string(),
                username: "app".to_string(),
                connect_string: "db:1521/PROD".to_string(),
                folder: String::new(),
                access: Default::default(),
                save_password: false,
            },
        ];
        save_profiles_in(&dir, &profiles).unwrap();
        assert_eq!(load_profiles_in(&dir), profiles);
        // Corrupt file -> empty list, not a crash.
        fs::write(dir.join("connections.json"), "{not json").unwrap();
        assert!(load_profiles_in(&dir).is_empty());
        // Entries missing a field are skipped, valid ones kept.
        fs::write(
            dir.join("connections.json"),
            r#"[{"name":"x"},{"name":"y","username":"u","connect_string":"c"}]"#,
        )
        .unwrap();
        assert_eq!(load_profiles_in(&dir).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profiles_file_never_holds_a_password() {
        let dir = scratch("profiles-nopw");
        let profiles = vec![SavedProfile {
            id: "p1".to_string(),
            name: "HR".to_string(),
            username: "hr".to_string(),
            connect_string: "db:1521/X".to_string(),
            folder: String::new(),
            access: Default::default(),
            save_password: true,
        }];
        save_profiles_in(&dir, &profiles).unwrap();
        let text = fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(!text.contains("\"password\""), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_profiles_get_stable_ids() {
        let dir = scratch("profiles-legacy");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("connections.json"),
            r#"[{"name":"a","username":"u","connect_string":"c"},
               {"id":"dup","name":"b","username":"u","connect_string":"c"},
               {"id":"dup","name":"c","username":"u","connect_string":"c"}]"#,
        )
        .unwrap();
        let first = load_profiles_in(&dir);
        assert_eq!(first.len(), 3);
        assert!(first.iter().all(|p| !p.id.is_empty() && !p.save_password));
        assert_eq!(first[1].id, "dup");
        assert_ne!(first[2].id, "dup");
        assert_ne!(first[0].id, first[2].id);
        // Ids were written back, so a second load sees the same ones.
        assert_eq!(load_profiles_in(&dir), first);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_ids_are_unique() {
        let ids: std::collections::HashSet<_> = (0..1000).map(|_| new_profile_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn history_round_trip_dedupe_and_cap() {
        let dir = scratch("history");
        let mut entries = Vec::new();
        record_history(&mut entries, "SELECT 1", 100);
        record_history(&mut entries, "SELECT 2", 101);
        record_history(&mut entries, "SELECT 1", 102);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sql, "SELECT 1");
        assert_eq!(entries[0].saved_at, 102);
        record_history(&mut entries, "   ", 103);
        assert_eq!(entries.len(), 2);
        entries[1].favorite = true;
        record_history(&mut entries, "SELECT 2", 104);
        assert!(entries[0].favorite, "re-run keeps favorite flag");
        for i in 0..60 {
            record_history(&mut entries, &format!("SELECT {i}"), 200 + i);
        }
        assert_eq!(entries.len(), MAX_HISTORY_ENTRIES);
        save_history_in(&dir, &entries).unwrap();
        assert_eq!(load_history_in(&dir), entries);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_preference_round_trip() {
        let dir = scratch("theme");
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::System);
        save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
        save_theme_preference_in(&dir, ThemePreference::Light).unwrap();
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Light);
        fs::write(dir.join("settings.json"), r#"{"theme":"bogus"}"#).unwrap();
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::System);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn panel_layout_round_trip_keeps_other_keys() {
        let dir = scratch("panel-layout");
        assert_eq!(load_panel_layout_in(&dir), PanelLayout::default());
        save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
        let layout = PanelLayout {
            info_collapsed: true,
            plan_collapsed: false,
        };
        save_panel_layout_in(&dir, layout).unwrap();
        assert_eq!(load_panel_layout_in(&dir), layout);
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_names_round_trip_keep_other_keys() {
        let dir = scratch("theme-names");
        assert_eq!(load_theme_names_in(&dir), ThemeNames::default());
        save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
        let names = ThemeNames {
            light: Some("Ayu Light".into()),
            dark: Some("Tokyo Night".into()),
        };
        save_theme_names_in(&dir, &names).unwrap();
        assert_eq!(load_theme_names_in(&dir), names);
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
        // Back to the app palette removes the key again.
        save_theme_names_in(
            &dir,
            &ThemeNames {
                light: None,
                ..names.clone()
            },
        )
        .unwrap();
        assert_eq!(load_theme_names_in(&dir).light, None);
        let text = fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(!text.contains("light_theme"), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_dir_is_app_specific() {
        let dir = config_dir();
        assert!(dir.ends_with("gpui-like-toad"));
        assert!(dir.is_absolute() || dir.starts_with("."));
    }

    #[test]
    fn update_prefs_roundtrip_keeps_theme() {
        let dir = scratch("update-prefs");
        save_theme_preference_in(&dir, ThemePreference::Dark).unwrap();
        assert_eq!(load_update_prefs_in(&dir), UpdatePrefs::default());
        let prefs = UpdatePrefs {
            auto_check: false,
            last_checked: 1_700_000_000,
            dismissed_version: Some("0.2.0".into()),
            last_launched_version: Some("0.1.0".into()),
        };
        save_update_prefs_in(&dir, &prefs).unwrap();
        assert_eq!(load_update_prefs_in(&dir), prefs);
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    }
}
