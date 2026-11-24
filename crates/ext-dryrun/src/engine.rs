//! The dry run itself: capture the rows a statement will touch, run the
//! statement without committing, read the rows again, compare, and write the
//! script that puts the old values back. All database work goes through
//! [`DbClient`], so the logic is tested without a database.

use ora_core::client::DbClient;
use ora_core::copy::{insert_statement, sql_literal};
use ora_core::import::quote_ident;
use ora_core::model::Value;
use ora_core::sql::clean_statement;
use ora_core::FetchLimits;

use crate::sql::{self, Kind, Parsed};

/// Rows captured before the statement runs. More than this and the dry run
/// still reports the exact count, but writes no undo script.
pub const CAPTURE_ROWS: usize = 1000;
/// Changed rows that are drawn in the view.
pub const SHOW_ROWS: usize = 200;
/// ROWIDs per `IN (...)` when the rows are read again.
const REREAD_CHUNK: usize = 100;
/// Text and binary cells longer than this are clipped (and left out of the
/// undo script).
const CELL_BYTES: usize = 4000;
const ROWID_COLUMN: &str = "KILAT_RID";
/// Seconds the capture waits for row locks. The dry run has its own session,
/// so rows another session changed and has not committed (often the user's
/// own editor) would make the statement wait forever.
const LOCK_WAIT_SECS: u32 = 10;
/// The view carries the script as one string, which the UI protocol caps at
/// 64 KiB. A longer script is not written (a cut one would look complete).
pub const UNDO_MAX_BYTES: usize = 60_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub rowid: String,
    pub old: Vec<Value>,
    /// `None`: the row is gone (DELETE), or an UPDATE could not find it again.
    pub new: Option<Vec<Value>>,
    /// Indexes of the columns whose value differs (UPDATE only).
    pub changed: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub sql: String,
    pub kind: Kind,
    pub table: String,
    pub affected: u64,
    pub columns: Vec<String>,
    /// Rows that changed, at most [`SHOW_ROWS`] of them.
    pub changes: Vec<Change>,
    /// How many of the captured rows changed (can exceed `changes.len()`).
    pub changed_rows: usize,
    /// Captured rows an UPDATE left as they were.
    pub unchanged_rows: usize,
    /// Captured rows an UPDATE could not find again (row movement).
    pub moved_rows: usize,
    pub captured: usize,
    /// More rows were affected than captured.
    pub partial: bool,
    pub triggers: Option<u64>,
    pub undo: Option<String>,
    /// Why the undo script is missing or incomplete.
    pub notes: Vec<String>,
}

fn limits() -> FetchLimits {
    FetchLimits {
        max_rows: CAPTURE_ROWS + 1,
        max_cell_bytes: CELL_BYTES,
        max_result_bytes: 64 * 1024 * 1024,
        read_lobs: true,
    }
}

fn valid_rowid(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 4000
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '*' | '.' | '-' | '_'))
}

fn rowid_of(value: &Value) -> Option<String> {
    match value {
        Value::Text(text) if valid_rowid(text) => Some(text.clone()),
        _ => None,
    }
}

fn select_rows(parsed: &Parsed, condition: Option<&str>) -> String {
    let q = parsed.qualifier();
    let mut sql = format!(
        "SELECT {q}.ROWID AS {ROWID_COLUMN}, {q}.* FROM {}",
        parsed.table_with_alias()
    );
    if let Some(condition) = condition {
        sql.push_str(" WHERE ");
        sql.push_str(condition);
    }
    sql
}

/// Number of enabled triggers on the table; `None` when it cannot be read
/// (no access to the dictionary view).
fn count_triggers(client: &mut dyn DbClient, parsed: &Parsed) -> Option<u64> {
    let (sql, binds) = match &parsed.owner {
        Some(owner) => (
            "SELECT COUNT(*) FROM ALL_TRIGGERS WHERE TABLE_OWNER = :1 AND TABLE_NAME = :2 \
             AND STATUS = 'ENABLED'",
            vec![owner.name(), parsed.table.name()],
        ),
        None => (
            "SELECT COUNT(*) FROM ALL_TRIGGERS WHERE TABLE_OWNER = \
             SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') AND TABLE_NAME = :1 \
             AND STATUS = 'ENABLED'",
            vec![parsed.table.name()],
        ),
    };
    let result = client.query_binds(sql, &binds, FetchLimits::rows(1)).ok()?;
    match result.rows.first()?.first()? {
        Value::Integer(n) => u64::try_from(*n).ok(),
        Value::Text(t) => t.trim().parse().ok(),
        Value::Float(f) if *f >= 0.0 => Some(*f as u64),
        _ => None,
    }
}

struct Captured {
    columns: Vec<String>,
    rows: Vec<(String, Vec<Value>)>,
    truncated: bool,
}

fn capture(client: &mut dyn DbClient, sql: &str, expect_rows: usize) -> Result<Captured, String> {
    let result = client.query(sql, limits()).map_err(|e| e.to_string())?;
    let columns: Vec<String> = result
        .columns
        .iter()
        .skip(1)
        .map(|c| c.name.clone())
        .collect();
    let mut rows = Vec::with_capacity(result.rows.len().min(expect_rows.max(1)));
    for mut row in result.rows {
        if row.is_empty() {
            continue;
        }
        let rowid =
            rowid_of(&row.remove(0)).ok_or("The database returned a row without a ROWID.")?;
        rows.push((rowid, row));
    }
    Ok(Captured {
        columns,
        rows,
        truncated: result.truncated,
    })
}

/// A clear message for a row lock timeout; any other error as it is.
fn locked_message(error: &str) -> String {
    if error.contains("ORA-30006") || error.contains("ORA-00054") {
        format!(
            "The rows are locked by another session that has not committed yet, often this \
             same connection in the SQL editor or a data tab. Commit or Roll back there, then \
             run Dry-run again. ({})",
            error.trim()
        )
    } else {
        error.to_string()
    }
}

/// Run `sql` as a dry run. The statement stays uncommitted in `client`'s
/// session; the caller rolls it back.
pub fn run(client: &mut dyn DbClient, statement: &str) -> Result<Report, String> {
    let sql = clean_statement(statement).to_string();
    let parsed = sql::parse(&sql)?;
    let triggers = count_triggers(client, &parsed);

    // Old values. `FOR UPDATE WAIT` locks the rows the statement will touch
    // (it would lock them anyway) and gives up when another session holds
    // them, instead of waiting without end in the statement itself.
    let capture_sql = format!(
        "{}\nFOR UPDATE WAIT {LOCK_WAIT_SECS}",
        select_rows(&parsed, parsed.where_clause.as_deref())
    );
    let old =
        capture(client, &capture_sql, CAPTURE_ROWS).map_err(|error| locked_message(&error))?;
    let truncated = old.truncated;
    let mut old_rows = old.rows;
    if truncated {
        old_rows.truncate(CAPTURE_ROWS);
    }

    // The statement. Nothing is committed here.
    let affected = client.execute(&sql).map_err(|e| e.to_string())?;

    // New values.
    let mut new_rows: std::collections::HashMap<String, Vec<Value>> = Default::default();
    if parsed.kind == Kind::Update {
        for chunk in old_rows.chunks(REREAD_CHUNK) {
            let list = chunk
                .iter()
                .map(|(rowid, _)| format!("CHARTOROWID('{rowid}')"))
                .collect::<Vec<_>>()
                .join(", ");
            let condition = format!("{}.ROWID IN ({list})", parsed.qualifier());
            let fresh = capture(client, &select_rows(&parsed, Some(&condition)), chunk.len())?;
            new_rows.extend(fresh.rows);
        }
    }

    let mut changes = Vec::new();
    let mut changed_rows = 0usize;
    let mut unchanged_rows = 0usize;
    let mut moved_rows = 0usize;
    let mut all_changes: Vec<Change> = Vec::new();
    for (rowid, before) in old_rows.iter() {
        let change = match parsed.kind {
            Kind::Delete => Change {
                rowid: rowid.clone(),
                old: before.clone(),
                new: None,
                changed: Vec::new(),
            },
            Kind::Update => match new_rows.get(rowid) {
                None => {
                    moved_rows += 1;
                    Change {
                        rowid: rowid.clone(),
                        old: before.clone(),
                        new: None,
                        changed: Vec::new(),
                    }
                }
                Some(after) => {
                    let changed: Vec<usize> = (0..before.len().min(after.len()))
                        .filter(|&i| before[i] != after[i])
                        .collect();
                    if changed.is_empty() {
                        unchanged_rows += 1;
                        continue;
                    }
                    Change {
                        rowid: rowid.clone(),
                        old: before.clone(),
                        new: Some(after.clone()),
                        changed,
                    }
                }
            },
        };
        changed_rows += 1;
        all_changes.push(change);
    }
    changes.extend(all_changes.iter().take(SHOW_ROWS).cloned());

    let partial = truncated || affected as usize > old_rows.len();
    let mut notes = Vec::new();
    let undo = if partial {
        notes.push(if truncated {
            format!(
                "More than {CAPTURE_ROWS} rows are affected, so the old values were not \
                 captured in full and no undo script is written. Rolling back this dry run \
                 changes nothing."
            )
        } else {
            format!(
                "The statement changed {affected} rows but {} were captured before it ran \
                 (another session changed data in between). No undo script is written.",
                old_rows.len()
            )
        });
        None
    } else {
        let script = undo_script(&parsed, &old.columns, &all_changes, &mut notes);
        if script.len() > UNDO_MAX_BYTES {
            notes.push(format!(
                "The undo script would be {} KB, more than the {} KB the view can carry, so it \
                 is not written. Narrow the WHERE clause to get one.",
                script.len() / 1000,
                UNDO_MAX_BYTES / 1000
            ));
            None
        } else {
            Some(script)
        }
    };
    if moved_rows > 0 {
        notes.push(format!(
            "{moved_rows} row(s) could not be read again after the UPDATE (the row moved). \
             They are listed without a new value and are not in the undo script."
        ));
    }

    Ok(Report {
        sql,
        kind: parsed.kind,
        table: parsed.table_ref(),
        affected,
        columns: old.columns,
        changes,
        changed_rows,
        unchanged_rows,
        moved_rows,
        captured: old_rows.len(),
        partial,
        triggers,
        undo,
        notes,
    })
}

fn clipped(value: &Value) -> bool {
    value.is_clipped()
}

/// The statements that put the old values back. Rows that cannot be written
/// faithfully (a clipped LOB) are listed in a comment instead.
fn undo_script(
    parsed: &Parsed,
    columns: &[String],
    changes: &[Change],
    notes: &mut Vec<String>,
) -> String {
    let table = parsed.table_ref();
    let mut lines = vec![format!(
        "-- Undo for: {} on {table}. Check it before you run it.",
        parsed.kind.label()
    )];
    match parsed.kind {
        Kind::Update => lines.push(
            "-- Valid while the rows keep their ROWID (no row movement or reorganizing).".into(),
        ),
        Kind::Delete => lines.push(
            "-- Remove virtual or identity-always columns from the INSERTs if the database \
             refuses them."
                .into(),
        ),
    }
    let mut skipped = 0usize;
    for change in changes {
        match parsed.kind {
            Kind::Update => {
                let Some(_) = &change.new else {
                    continue;
                };
                if change.changed.iter().any(|&i| clipped(&change.old[i])) {
                    skipped += 1;
                    lines.push(format!(
                        "-- ROWID {}: a changed value is too large to include, restore it by hand.",
                        change.rowid
                    ));
                    continue;
                }
                let sets = change
                    .changed
                    .iter()
                    .map(|&i| {
                        format!(
                            "{} = {}",
                            quote_ident(&columns[i]),
                            sql_literal(&change.old[i])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "UPDATE {table} SET {sets} WHERE ROWID = CHARTOROWID('{}');",
                    change.rowid
                ));
            }
            Kind::Delete => {
                if change.old.iter().any(clipped) {
                    skipped += 1;
                    lines.push(format!(
                        "-- ROWID {}: a value is too large to include, restore this row by hand.",
                        change.rowid
                    ));
                    continue;
                }
                let names: Vec<&str> = columns.iter().map(String::as_str).collect();
                let values: Vec<&Value> = change.old.iter().collect();
                lines.push(insert_statement(&table, &names, &values));
            }
        }
    }
    if skipped > 0 {
        notes.push(format!(
            "{skipped} row(s) hold values over {CELL_BYTES} bytes that the undo script cannot \
             restore. They are listed as comments."
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
pub(crate) mod tests;
