//! Cell edits from the data grid: one `UPDATE` per changed cell, found by
//! `ROWID`. Values (and the ROWID) travel as bind variables; identifiers
//! are quoted.

use crate::filter::BoundQuery;
use crate::import::quote_ident;

/// `UPDATE owner.table SET column = :1 WHERE ROWID = CHARTOROWID(:2)`, or
/// `SET column = NULL` when `value` is `None` (then the ROWID is `:1`).
pub fn update_cell_sql(
    owner: &str,
    table: &str,
    column: &str,
    rowid: &str,
    value: Option<&str>,
) -> BoundQuery {
    let target = format!("{}.{}", quote_ident(owner), quote_ident(table));
    let column = quote_ident(column);
    match value {
        Some(v) => BoundQuery {
            sql: format!("UPDATE {target} SET {column} = :1 WHERE ROWID = CHARTOROWID(:2)"),
            binds: vec![v.to_string(), rowid.to_string()],
        },
        None => BoundQuery {
            sql: format!("UPDATE {target} SET {column} = NULL WHERE ROWID = CHARTOROWID(:1)"),
            binds: vec![rowid.to_string()],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_binds_value_and_rowid() {
        let q = update_cell_sql("HR", "EMP", "LAST_NAME", "AAAB", Some("O'Hara"));
        assert_eq!(
            q.sql,
            "UPDATE \"HR\".\"EMP\" SET \"LAST_NAME\" = :1 WHERE ROWID = CHARTOROWID(:2)"
        );
        assert_eq!(q.binds, ["O'Hara", "AAAB"]);
    }

    #[test]
    fn null_update_has_no_value_bind() {
        let q = update_cell_sql("HR", "EMP", "COL\"X", "AAAB", None);
        assert_eq!(
            q.sql,
            "UPDATE \"HR\".\"EMP\" SET \"COL\"\"X\" = NULL WHERE ROWID = CHARTOROWID(:1)"
        );
        assert_eq!(q.binds, ["AAAB"]);
    }
}
