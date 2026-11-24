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
}

pub struct State<'a> {
    pub revision: u32,
    pub phase: &'a Phase,
    pub report: Option<&'a Report>,
    pub tab: &'a str,
    /// Time left before the automatic rollback, while pending.
    pub remaining: Option<Duration>,
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
            let mut row = vec![json!(change.rowid)];
            for &i in &columns {
                let old = change.old.get(i).map(short).unwrap_or_default();
                let cell = match (&change.new, change.changed.contains(&i)) {
                    (Some(new), true) => {
                        format!("{old} → {}", new.get(i).map(short).unwrap_or_default())
                    }
                    _ => old,
                };
                row.push(json!(cell));
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
    children
        .push(json!({"type": "scroll", "child": {"type": "table", "columns": head, "rows": rows}}));
    json!({"type": "column", "children": children})
}

fn text(style: &str, tone: &str, text: &str) -> Json {
    json!({"type": "text", "style": style, "tone": tone, "text": text})
}

pub fn build(state: &State) -> Json {
    let mut main = Vec::new();
    match state.report {
        None => main.push(json!({"type": "empty", "text": "Nothing has run yet."})),
        Some(report) => {
            main.push(text(
                "heading",
                "normal",
                &format!("Dry-run: {} {}", report.kind.label(), report.table),
            ));
            match state.phase {
                Phase::Pending => {
                    let left = state
                        .remaining
                        .map(|d| format!(" Rolls back by itself in {}.", countdown(d)))
                        .unwrap_or_default();
                    main.push(text(
                        "body",
                        "warning",
                        &format!("Not committed.{left} Rows stay locked until then."),
                    ));
                }
                Phase::RolledBack(why) => main.push(text(
                    "body",
                    "success",
                    &format!("Rolled back {why}. The database is unchanged."),
                )),
                Phase::Idle => {}
            }
            let mut rows = vec![
                json!(["Rows affected", report.affected.to_string()]),
                json!(["Rows that change", report.changed_rows.to_string()]),
            ];
            if report.kind == Kind::Update {
                rows.push(json!([
                    "Rows left as they were",
                    report.unchanged_rows.to_string()
                ]));
            }
            main.push(json!({"type": "kv", "rows": rows}));
            if let Some(n) = report.triggers.filter(|n| *n > 0) {
                main.push(text(
                    "body",
                    "warning",
                    &format!(
                        "The table has {n} enabled trigger(s). Work they do outside the \
                         transaction (autonomous transactions, mail, database links) is not rolled back."
                    ),
                ));
            }
            for note in &report.notes {
                main.push(text("body", "warning", note));
            }
            let mut items = vec![json!({"id": "changes", "label": "Changes",
                "child": changes_table(report)})];
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
            main.push(json!({"type": "row", "children": [
                {"type": "button", "id": "rollback", "label": "Roll back now",
                 "variant": "secondary", "disabled": *state.phase != Phase::Pending},
                {"type": "button", "id": "run-for-real", "label": "Run for real",
                 "variant": "primary", "writes": true,
                 "confirm": "Run this statement for real in your editor session? It is not rolled back."},
            ]}));
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
        for phase in [Phase::Pending, Phase::RolledBack("by you".into())] {
            let view = build(&State {
                revision: 3,
                phase: &phase,
                report: Some(&report),
                tab: "undo",
                remaining: Some(Duration::from_secs(272)),
            });
            let update = decode_update(&view.to_string(), Origin::Open).unwrap();
            assert_eq!(update.revision, 3);
            assert_eq!(update.poll_ms.is_some(), phase == Phase::Pending);
        }
    }

    #[test]
    fn update_rows_show_old_arrow_new() {
        let report = report();
        let view = build(&State {
            revision: 1,
            phase: &Phase::Pending,
            report: Some(&report),
            tab: "changes",
            remaining: Some(Duration::from_secs(300)),
        })
        .to_string();
        assert!(view.contains("100 → 110"), "{view}");
        assert!(view.contains("5:00"));
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
        });
        decode_update(&view.to_string(), Origin::Open).unwrap();
        assert!(view
            .to_string()
            .contains("Showing the first 200 of 500 rows."));
    }
}
