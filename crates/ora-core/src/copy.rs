//! Text a grid row is copied as: comma lists, tab separated values and
//! INSERT / UPDATE statements for the clipboard. The statements are for
//! pasting into an editor, never executed by the app, so values are written
//! as literals.

use crate::model::Value;

/// A value as an SQL literal: `NULL`, a number, `'text'` with quotes
/// doubled, or `HEXTORAW('..')` for binary data.
pub fn sql_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Integer(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Bytes(b) => {
            let hex: String = b.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("HEXTORAW('{hex}')")
        }
    }
}

/// A value as plain text for a delimited list; NULL is an empty string.
pub fn plain(value: &Value) -> String {
    match value {
        Value::Bytes(b) => b.iter().map(|byte| format!("{byte:02X}")).collect(),
        other => other.display(),
    }
}

/// Tabs and line breaks would break the grid layout of a pasted TSV.
fn tsv_cell(text: String) -> String {
    text.replace(['\t', '\r', '\n'], " ")
}

/// Cells joined by `sep` (`", "` for the comma lists).
pub fn join_values(values: &[&Value], sep: &str) -> String {
    values
        .iter()
        .map(|v| plain(v))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Tab separated values. `names` and/or `values` may be omitted.
pub fn tsv(names: Option<&[&str]>, values: Option<&[&Value]>) -> String {
    let mut lines = Vec::new();
    if let Some(names) = names {
        lines.push(
            names
                .iter()
                .map(|n| tsv_cell((*n).to_string()))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    if let Some(values) = values {
        lines.push(
            values
                .iter()
                .map(|v| tsv_cell(plain(v)))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.join("\n")
}

fn quote(name: &str) -> String {
    crate::import::quote_ident(name)
}

/// `INSERT INTO table (cols) VALUES (literals);`
pub fn insert_statement(table: &str, names: &[&str], values: &[&Value]) -> String {
    format!(
        "INSERT INTO {} ({}) VALUES ({});",
        table,
        names
            .iter()
            .map(|n| quote(n))
            .collect::<Vec<_>>()
            .join(", "),
        values
            .iter()
            .map(|v| sql_literal(v))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// `UPDATE table SET col = literal, ... WHERE ROWID = 'x';` When the row's
/// ROWID is unknown the WHERE clause lists every non-NULL column instead
/// (NULL columns as `IS NULL`).
pub fn update_statement(
    table: &str,
    names: &[&str],
    values: &[&Value],
    rowid: Option<&str>,
) -> String {
    let sets = names
        .iter()
        .zip(values)
        .map(|(n, v)| format!("{} = {}", quote(n), sql_literal(v)))
        .collect::<Vec<_>>()
        .join(", ");
    let condition = match rowid {
        Some(id) => format!("ROWID = '{}'", id.replace('\'', "''")),
        None => names
            .iter()
            .zip(values)
            .map(|(n, v)| {
                if v.is_null() {
                    format!("{} IS NULL", quote(n))
                } else {
                    format!("{} = {}", quote(n), sql_literal(v))
                }
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    };
    format!("UPDATE {table} SET {sets} WHERE {condition};")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Vec<Value> {
        vec![
            Value::Integer(7),
            Value::Text("O'Brien".into()),
            Value::Null,
        ]
    }

    #[test]
    fn insert_quotes_text_and_keeps_null() {
        let r = row();
        let v: Vec<&Value> = r.iter().collect();
        assert_eq!(
            insert_statement("\"HR\".\"EMP\"", &["ID", "NAME", "BOSS"], &v),
            "INSERT INTO \"HR\".\"EMP\" (\"ID\", \"NAME\", \"BOSS\") VALUES (7, 'O''Brien', NULL);"
        );
    }

    #[test]
    fn update_prefers_rowid_then_falls_back_to_columns() {
        let r = row();
        let v: Vec<&Value> = r.iter().collect();
        let names = ["ID", "NAME", "BOSS"];
        assert_eq!(
            update_statement("T", &names, &v, Some("AAAB")),
            "UPDATE T SET \"ID\" = 7, \"NAME\" = 'O''Brien', \"BOSS\" = NULL WHERE ROWID = 'AAAB';"
        );
        assert!(update_statement("T", &names, &v, None)
            .ends_with("WHERE \"ID\" = 7 AND \"NAME\" = 'O''Brien' AND \"BOSS\" IS NULL;"));
    }

    #[test]
    fn lists_and_tsv() {
        let r = row();
        let v: Vec<&Value> = r.iter().collect();
        assert_eq!(join_values(&v, ", "), "7, O'Brien, ");
        assert_eq!(
            tsv(Some(&["ID", "NAME", "BOSS"]), Some(&v)),
            "ID\tNAME\tBOSS\n7\tO'Brien\t"
        );
        assert_eq!(tsv(Some(&["A\tB"]), None), "A B");
    }
}
