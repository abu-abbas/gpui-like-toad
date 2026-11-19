//! Query history.

use super::*;

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
