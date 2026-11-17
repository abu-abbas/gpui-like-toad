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
    let value = p.parse_value()?;
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

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
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

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.next() {
                Some(',') => {}
                Some(']') => return Ok(Json::Arr(items)),
                _ => return Err(format!("expected ',' or ']' at offset {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
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
            let value = self.parse_value()?;
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

pub fn load_theme_preference() -> ThemePreference {
    load_theme_preference_in(&config_dir())
}

pub fn save_theme_preference(pref: ThemePreference) -> std::io::Result<()> {
    save_theme_preference_in(&config_dir(), pref)
}

// ---------------------------------------------------------------------------
// Update-check preferences
// ---------------------------------------------------------------------------

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
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_checked: 0,
            dismissed_version: None,
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
                save_password: true,
            },
            SavedProfile {
                id: "p2".to_string(),
                name: "Prod \"A\"".to_string(),
                username: "app".to_string(),
                connect_string: "db:1521/PROD".to_string(),
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
        };
        save_update_prefs_in(&dir, &prefs).unwrap();
        assert_eq!(load_update_prefs_in(&dir), prefs);
        assert_eq!(load_theme_preference_in(&dir), ThemePreference::Dark);
    }
}
