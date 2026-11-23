//! Row identity for the data tab.
//!
//! A table with a primary key identifies its rows by it: the id of a row is
//! the JSON array of its key values (`[1,"a"]`), which stays valid after the
//! row is updated. A table without one falls back to `ctid` (`(0,1)`), which
//! changes whenever the row is updated, so a second edit of the same row
//! before a refresh finds nothing and says so.

use ora_core::Dialect;

use crate::catalog::KeyColumn;

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// SELECT-list expression giving the row id of alias `t`.
pub(crate) fn id_expr(key: &[KeyColumn]) -> String {
    let cols: Vec<String> = key
        .iter()
        .map(|k| format!("t.{}", quote(&k.name)))
        .collect();
    format!("jsonb_build_array({})::text", cols.join(", "))
}

/// Condition that matches the row whose id is bind `:bind`.
pub(crate) fn matches(dialect: &dyn Dialect, id: &str, key: &[KeyColumn], bind: usize) -> String {
    if !id.starts_with('[') || key.is_empty() {
        return dialect.row_id_match(bind);
    }
    key.iter()
        .enumerate()
        .map(|(i, k)| {
            // The type name comes from the catalog (`regtype`), quoted
            // where it needs to be.
            format!(
                "{} = (CAST(:{bind} AS jsonb) ->> {i})::{}",
                quote(&k.name),
                k.ty
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ora_core::dialect::PostgresDialect;

    fn key() -> Vec<KeyColumn> {
        vec![
            KeyColumn {
                name: "id".into(),
                ty: "integer".into(),
            },
            KeyColumn {
                name: "co\"de".into(),
                ty: "text".into(),
            },
        ]
    }

    #[test]
    fn keys_select_and_match_by_json() {
        assert_eq!(
            id_expr(&key()),
            "jsonb_build_array(t.\"id\", t.\"co\"\"de\")::text"
        );
        assert_eq!(
            matches(&PostgresDialect, "[1,\"a\"]", &key(), 2),
            "\"id\" = (CAST(:2 AS jsonb) ->> 0)::integer AND \
             \"co\"\"de\" = (CAST(:2 AS jsonb) ->> 1)::text"
        );
    }

    #[test]
    fn a_ctid_or_keyless_table_matches_by_ctid() {
        assert_eq!(
            matches(&PostgresDialect, "(0,3)", &key(), 1),
            "ctid = CAST(CAST(:1 AS text) AS tid)"
        );
        assert_eq!(
            matches(&PostgresDialect, "[1]", &[], 1),
            "ctid = CAST(CAST(:1 AS text) AS tid)"
        );
    }
}
