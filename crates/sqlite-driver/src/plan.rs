//! `EXPLAIN QUERY PLAN` as the app's plan tree. SQLite reports one row per
//! step with `id`, `parent` (0 = top level) and a `detail` text such as
//! `SCAN emp` or `SEARCH emp USING INDEX emp_dept (dept_id=?)`.

use ora_core::sql::clean_statement;
use ora_core::{DbError, PlanNode};
use rusqlite::Connection;

use crate::db_err;

pub(crate) fn nodes(conn: &Connection, sql: &str) -> Result<Vec<PlanNode>, DbError> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", clean_statement(sql)))
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let parent: i64 = r.get(1)?;
            let detail: String = r.get(3)?;
            Ok((id, parent, detail))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(rows
        .into_iter()
        .map(|(id, parent, detail)| {
            let (operation, rest) = detail.split_once(' ').unwrap_or((detail.as_str(), ""));
            PlanNode {
                id,
                parent_id: (parent != 0).then_some(parent),
                operation: operation.to_string(),
                options: rest.to_string(),
                object_name: String::new(),
                cost: None,
                cardinality: None,
            }
        })
        .collect())
}

/// The plan as indented text lines, one per step.
pub(crate) fn as_text(nodes: &[PlanNode]) -> Vec<String> {
    let depth = |node: &PlanNode| {
        let mut depth = 0;
        let mut parent = node.parent_id;
        while let Some(p) = parent {
            depth += 1;
            parent = nodes.iter().find(|n| n.id == p).and_then(|n| n.parent_id);
            if depth > nodes.len() {
                break;
            }
        }
        depth
    };
    nodes
        .iter()
        .map(|n| format!("{}{}", "  ".repeat(depth(n)), n.label()))
        .collect()
}
