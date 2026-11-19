//! Row edits from the data grid: one `UPDATE` per changed cell and one
//! `DELETE` per removed row, both found by row id, plus `INSERT` for new
//! rows. Values (and the row id) travel as bind variables; identifiers are
//! quoted. The row-id syntax comes from the connection's [`Dialect`].

use crate::dialect::Dialect;
use crate::filter::BoundQuery;

/// `UPDATE owner.table SET column = :1 WHERE <row id> = :2`, or
/// `SET column = NULL` when `value` is `None` (then the row id is `:1`).
pub fn update_cell_sql(
    dialect: &dyn Dialect,
    owner: &str,
    table: &str,
    column: &str,
    rowid: &str,
    value: Option<&str>,
) -> BoundQuery {
    let target = dialect.table_ref(owner, table);
    let column = dialect.quote_ident(column);
    match value {
        Some(v) => BoundQuery {
            sql: format!(
                "UPDATE {target} SET {column} = :1 WHERE {}",
                dialect.row_id_match(2)
            ),
            binds: vec![v.to_string(), rowid.to_string()],
        },
        None => BoundQuery {
            sql: format!(
                "UPDATE {target} SET {column} = NULL WHERE {}",
                dialect.row_id_match(1)
            ),
            binds: vec![rowid.to_string()],
        },
    }
}

/// `DELETE FROM owner.table WHERE <row id> = :1`.
pub fn delete_row_sql(dialect: &dyn Dialect, owner: &str, table: &str, rowid: &str) -> BoundQuery {
    BoundQuery {
        sql: format!(
            "DELETE FROM {} WHERE {}",
            dialect.table_ref(owner, table),
            dialect.row_id_match(1)
        ),
        binds: vec![rowid.to_string()],
    }
}

/// `INSERT INTO owner.table (c1, c2) VALUES (:1, :2)`, values bound as
/// text. `None` when `fields` is empty.
pub fn insert_row_sql(
    dialect: &dyn Dialect,
    owner: &str,
    table: &str,
    fields: &[(String, String)],
) -> Option<BoundQuery> {
    if fields.is_empty() {
        return None;
    }
    let columns = fields
        .iter()
        .map(|(c, _)| dialect.quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let params = (1..=fields.len())
        .map(|n| format!(":{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(BoundQuery {
        sql: format!(
            "INSERT INTO {} ({columns}) VALUES ({params})",
            dialect.table_ref(owner, table)
        ),
        binds: fields.iter().map(|(_, v)| v.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{OracleDialect, SqliteDialect};

    #[test]
    fn update_binds_value_and_rowid() {
        let q = update_cell_sql(
            &OracleDialect,
            "HR",
            "EMP",
            "LAST_NAME",
            "AAAB",
            Some("O'Hara"),
        );
        assert_eq!(
            q.sql,
            "UPDATE \"HR\".\"EMP\" SET \"LAST_NAME\" = :1 WHERE ROWID = CHARTOROWID(:2)"
        );
        assert_eq!(q.binds, ["O'Hara", "AAAB"]);
    }

    #[test]
    fn null_update_has_no_value_bind() {
        let q = update_cell_sql(&OracleDialect, "HR", "EMP", "COL\"X", "AAAB", None);
        assert_eq!(
            q.sql,
            "UPDATE \"HR\".\"EMP\" SET \"COL\"\"X\" = NULL WHERE ROWID = CHARTOROWID(:1)"
        );
        assert_eq!(q.binds, ["AAAB"]);
    }

    #[test]
    fn delete_and_insert_follow_the_dialect() {
        assert_eq!(
            delete_row_sql(&OracleDialect, "HR", "EMP", "AAAB").sql,
            "DELETE FROM \"HR\".\"EMP\" WHERE ROWID = CHARTOROWID(:1)"
        );
        assert_eq!(
            delete_row_sql(&SqliteDialect, "main", "emp", "7").sql,
            "DELETE FROM \"main\".\"emp\" WHERE rowid = :1"
        );
        let insert = insert_row_sql(
            &SqliteDialect,
            "main",
            "emp",
            &[("id".into(), "1".into()), ("name".into(), "Ana".into())],
        )
        .unwrap();
        assert_eq!(
            insert.sql,
            "INSERT INTO \"main\".\"emp\" (\"id\", \"name\") VALUES (:1, :2)"
        );
        assert_eq!(insert.binds, ["1", "Ana"]);
        assert!(insert_row_sql(&OracleDialect, "HR", "EMP", &[]).is_none());
    }

    #[test]
    fn sqlite_update_matches_plain_rowid() {
        let q = update_cell_sql(&SqliteDialect, "main", "t", "a", "3", Some("x"));
        assert_eq!(
            q.sql,
            "UPDATE \"main\".\"t\" SET \"a\" = :1 WHERE rowid = :2"
        );
    }
}
