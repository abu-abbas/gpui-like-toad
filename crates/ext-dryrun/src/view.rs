//! The Dry-run dialog as a declarative view (see
//! `docs/extension-ui-protocol.md`). Kilat draws it; nothing here draws.

use std::time::Duration;

use ora_core::extension_ui::VERSION;
use ora_core::model::Value;
use serde_json::{json, Value as Json};

use crate::engine::{Report, SHOW_ROWS};
use crate::sql::Kind;

/// Columns drawn in the changes table next to the ROWID.
const MAX_COLUMNS: usize = 10;
/// The protocol allows 5,000 nodes in a reply; the table gets most of them.
const CELL_BUDGET: usize = 3_800;
const CELL_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing has run yet.
    Idle,
    /// The statement ran and is waiting, uncommitted.
    Pending,
    /// Rolled back; the text says why ("by you", "after 5 minutes").
    RolledBack(String),
    /// Committed from the Dry-run tab.
    Committed,
}

pub struct State<'a> {
    pub revision: u32,
    pub phase: &'a Phase,
    pub report: Option<&'a Report>,
    pub tab: &'a str,
    /// Time left before the automatic rollback, while pending.
    pub remaining: Option<Duration>,
    /// How long the automatic rollback waits in all.
    pub hold: Duration,
    /// How long the rows have been locked, while pending.
    pub locked: Option<Duration>,
}

fn short(value: &Value) -> String {
    let text = match value {
        Value::Null => "NULL".to_string(),
        other => other.display(),
    };
    let text = text.replace(['\r', '\n', '\t'], " ");
    if text.chars().count() > CELL_CHARS {
        let cut: String = text.chars().take(CELL_CHARS).collect();
        format!("{cut}…")
    } else {
        text
    }
}

fn countdown(left: Duration) -> String {
    let secs = left.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Columns to draw: for an UPDATE the first column (usually the key) and
/// every column that changed in a shown row; for a DELETE the first ones.
fn shown_columns(report: &Report) -> Vec<usize> {
    let total = report.columns.len();
    if report.kind == Kind::Delete {
        return (0..total.min(MAX_COLUMNS)).collect();
    }
    let mut picked: Vec<usize> = Vec::new();
    if total > 0 {
        picked.push(0);
    }
    for change in &report.changes {
        for &i in &change.changed {
            if !picked.contains(&i) {
                picked.push(i);
            }
        }
    }
    picked.sort_unstable();
    picked.truncate(MAX_COLUMNS);
    picked
}

fn changes_table(report: &Report) -> Json {
    if report.changed_rows == 0 && report.moved_rows == 0 {
        return json!({"type": "empty", "text": match report.kind {
            Kind::Update => "No row would change.",
            Kind::Delete => "No row would be deleted.",
        }});
    }
    let columns = shown_columns(report);
    let per_row = columns.len() + 2;
    let rows_shown = report
        .changes
        .len()
        .min(SHOW_ROWS)
        .min(CELL_BUDGET / per_row);
    let mut head = vec![json!("ROWID")];
    head.extend(columns.iter().map(|&i| json!(report.columns[i])));
    let rows: Vec<Json> = report
        .changes
        .iter()
        .take(rows_shown)
        .map(|change| {
            let mut row = vec![json!({"text": change.rowid})];
            for &i in &columns {
                let old = change.old.get(i).map(short).unwrap_or_default();
                let cell = match (&change.new, change.changed.contains(&i)) {
                    (Some(new), true) => json!({
                        "text": new.get(i).map(short).unwrap_or_default(),
                        "old": old,
                    }),
                    // A deleted row, or an updated row that moved.
                    (None, _) => json!({"text": old, "removed": true}),
                    _ => json!({"text": old}),
                };
                row.push(cell);
            }
            Json::Array(row)
        })
        .collect();
    let mut children = Vec::new();
    if report.changed_rows > rows_shown {
        children.push(json!({"type": "text", "style": "caption", "tone": "muted",
            "text": format!("Showing the first {rows_shown} of {} rows.", report.changed_rows)}));
    }
    if report.columns.len() > columns.len() && report.kind == Kind::Delete {
        children.push(json!({"type": "text", "style": "caption", "tone": "muted",
            "text": format!("Showing {} of {} columns.", columns.len(), report.columns.len())}));
    }
    children.push(json!({"type": "diff", "columns": head, "rows": rows}));
    json!({"type": "column", "children": children})
}

fn text(style: &str, tone: &str, text: &str) -> Json {
    json!({"type": "text", "style": style, "tone": tone, "text": text})
}

fn rows(n: impl std::fmt::Display) -> String {
    format!("{n} row(s)")
}

/// The strip above the result: state, counts, timers and the two actions.
fn banner(state: &State, report: &Report) -> Json {
    let mut facts = vec![format!("{} in {}", rows(report.changed_rows), report.table)];
    if report.affected as usize != report.changed_rows {
        facts.push(format!("{} affected", report.affected));
    }
    if let Some(n) = report.triggers.filter(|n| *n > 0) {
        facts.push(format!("{n} enabled trigger(s)"));
    }
    match state.phase {
        Phase::Pending => {
            facts.push("own session, rows are locked".into());
            let mut children = Vec::new();
            if let Some(locked) = state.locked {
                children.push(json!({"type": "badge", "tone": "warning",
                    "text": format!("Lock {}", countdown(locked))}));
            }
            if let Some(left) = state.remaining {
                let total = state.hold.as_secs_f32().max(1.0);
                let value = (left.as_secs_f32() / total).clamp(0.0, 1.0);
                children.push(
                    json!({"type": "progress", "tone": "warning", "value": value,
                    "label": "Auto rollback", "trailing": countdown(left)}),
                );
            }
            children.push(
                json!({"type": "button", "id": "rollback", "label": "Roll back",
                "variant": "danger", "icon": "undo"}),
            );
            children.push(json!({"type": "button", "id": "commit", "label": "Commit",
            "variant": "success", "icon": "check", "writes": true,
            "confirm": format!(
                "Commit this dry run? {} in {} are written for good.",
                rows(report.affected), report.table
            )}));
            json!({"type": "banner", "tone": "warning", "icon": "flask",
                "title": "Dry-run is waiting · not committed",
                "text": facts.join(" · "), "children": children})
        }
        Phase::Committed => {
            let mut children = Vec::new();
            if report.undo.is_some() {
                children.push(json!({"type": "button", "id": "copy-undo",
                    "label": "Copy undo script", "icon": "copy", "variant": "secondary"}));
            }
            json!({"type": "banner", "tone": "success", "icon": "check",
                "title": "Committed",
                "text": facts.join(" · "), "children": children})
        }
        Phase::RolledBack(why) => json!({"type": "banner", "tone": "muted", "icon": "undo",
            "title": format!("Rolled back {why}"),
            "text": "The database is unchanged. Run Dry-run again to see fresh values."}),
        Phase::Idle => json!({"type": "banner", "tone": "muted", "icon": "flask",
            "title": "Dry-run", "text": "Nothing has run yet."}),
    }
}

pub fn build(state: &State) -> Json {
    let mut main = Vec::new();
    match state.report {
        None => main.push(json!({"type": "empty", "text": "Nothing has run yet."})),
        Some(report) => {
            main.push(banner(state, report));
            for note in &report.notes {
                main.push(text("body", "warning", note));
            }
            let mut changes = vec![json!({
                "type": "card", "icon": "table", "title": report.table,
                "badge": report.kind.label(),
                "badge_tone": match report.kind { Kind::Update => "warning", Kind::Delete => "danger" },
                "subtitle": match report.kind {
                    Kind::Update => format!("{} · only changed columns", rows(report.changed_rows)),
                    Kind::Delete => format!("{} removed", rows(report.changed_rows)),
                },
                "child": changes_table(report),
            })];
            if let Some(n) = report.triggers.filter(|n| *n > 0) {
                changes.push(text("caption", "muted", "SIDE EFFECTS · TRIGGERS"));
                changes.push(json!({
                    "type": "card", "icon": "zap",
                    "title": format!("{n} enabled trigger(s) on {}", report.table),
                    "badge": "NOT SHOWN", "badge_tone": "warning",
                    "subtitle": "Rows they change are not listed. Work they do outside the \
                        transaction (autonomous transactions, mail, database links) is not \
                        rolled back.",
                }));
            }
            let mut items = vec![json!({"id": "changes", "label": "Changes",
                "child": {"type": "column", "children": changes}})];
            items.push(json!({"id": "undo", "label": "Undo script", "child": match &report.undo {
                Some(script) => json!({"type": "code", "language": "sql", "text": script, "copy": true}),
                None => json!({"type": "empty", "text": "No undo script for this statement."}),
            }}));
            items.push(json!({"id": "statement", "label": "Statement",
                "child": {"type": "code", "language": "sql", "text": report.sql, "copy": true}}));
            let active = if items.iter().any(|i| i["id"] == state.tab) {
                state.tab
            } else {
                "changes"
            };
            main.push(json!({"type": "tabs", "id": "view", "active": active, "items": items}));
            main.push(text(
                "caption",
                "muted",
                "Dry-run uses its own session: it does not see changes you made in the editor \
                 and have not committed.",
            ));
        }
    }
    json!({
        "version": VERSION,
        "revision": state.revision,
        "slots": [{"slot": "main", "tree": {"type": "column", "children": main}}],
        "poll_ms": (*state.phase == Phase::Pending).then_some(1000),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::{dept10, raise, Fake};
    use ora_core::extension_ui::{decode_update, Origin};

    fn report() -> Report {
        let mut db = Fake::new(dept10, raise);
        crate::engine::run(
            &mut db,
            "UPDATE emp SET salary = salary + 10 WHERE dept = 10",
        )
        .unwrap()
    }

    #[test]
    fn the_view_passes_the_hosts_validator() {
        let report = report();
        for phase in [
            Phase::Pending,
            Phase::RolledBack("by you".into()),
            Phase::Committed,
        ] {
            let view = build(&State {
                revision: 3,
                phase: &phase,
                report: Some(&report),
                tab: "undo",
                remaining: Some(Duration::from_secs(272)),
                hold: Duration::from_secs(300),
                locked: Some(Duration::from_secs(28)),
            });
            let update = decode_update(&view.to_string(), Origin::Open).unwrap();
            assert_eq!(update.revision, 3);
            assert_eq!(update.poll_ms.is_some(), phase == Phase::Pending);
        }
    }

    #[test]
    fn update_rows_show_old_and_new_values() {
        let report = report();
        let view = build(&State {
            revision: 1,
            phase: &Phase::Pending,
            report: Some(&report),
            tab: "changes",
            remaining: Some(Duration::from_secs(300)),
            hold: Duration::from_secs(300),
            locked: Some(Duration::from_secs(0)),
        })
        .to_string();
        // Key order depends on serde_json's preserve_order, which the
        // workspace build turns on through another crate.
        assert!(
            view.contains(r#"{"old":"100","text":"110"}"#)
                || view.contains(r#"{"text":"110","old":"100"}"#),
            "{view}"
        );
        assert!(view.contains("5:00"));
        assert!(view.contains(r#""id":"commit""#), "{view}");
        // The unchanged NAME column is not drawn, the key is.
        assert!(view.contains(r#"["ROWID","ID","SALARY"]"#), "{view}");
    }

    #[test]
    fn an_empty_result_and_an_unknown_tab_still_validate() {
        let mut db = Fake::new(|_| false, |_| 0);
        let report = crate::engine::run(&mut db, "DELETE FROM emp WHERE 1 = 0").unwrap();
        let view = build(&State {
            revision: 1,
            phase: &Phase::Pending,
            report: Some(&report),
            tab: "nope",
            remaining: None,
            hold: Duration::from_secs(300),
            locked: None,
        });
        decode_update(&view.to_string(), Origin::Open).unwrap();
        assert!(view.to_string().contains("No row would be deleted."));
    }

    #[test]
    fn two_hundred_wide_rows_stay_inside_the_node_budget() {
        let mut db = Fake::new(|_| true, |rows| rows.len() as u64);
        db.rows = (0..500)
            .map(|n| (format!("R{n}"), vec![Value::Integer(n); 4]))
            .collect();
        let report = crate::engine::run(&mut db, "DELETE FROM emp").unwrap();
        let view = build(&State {
            revision: 1,
            phase: &Phase::Pending,
            report: Some(&report),
            tab: "changes",
            remaining: None,
            hold: Duration::from_secs(300),
            locked: None,
        });
        decode_update(&view.to_string(), Origin::Open).unwrap();
        assert!(view
            .to_string()
            .contains("Showing the first 200 of 500 rows."));
    }
}
