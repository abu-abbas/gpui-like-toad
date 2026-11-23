//! Row identity for the data tab.
//!
//! MySQL has no physical row id. The id of a row is the JSON array of its
//! primary key values as text (`["1","a"]`), which stays valid after the
//! row is updated. A table without a primary key is found by all its
//! columns that compare exactly (see [`RowKey`]); with none of those it
//! gets no id and is read-only.

use ora_core::store::{parse_json, Json};

use crate::conv::quote;

/// How the rows of a table are found again.
#[derive(Debug, Clone, Default)]
pub(crate) struct RowKey {
    pub columns: Vec<KeyColumn>,
    /// No primary key: the "key" is every column that compares reliably.
    /// Identical rows cannot be told apart, so a change touches one of
    /// them (`LIMIT 1`), and a changed value makes the id stale until the
    /// next refresh.
    pub all_columns: bool,
}

impl RowKey {
    pub fn limit_one(&self) -> &'static str {
        if self.all_columns {
            " LIMIT 1"
        } else {
            ""
        }
    }
}

/// One column of a primary key, with the cast that keeps a bound text
/// value exact against its column (a big integer must not pass through a
/// floating-point comparison).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyColumn {
    pub name: String,
    pub cast: Option<String>,
}

impl KeyColumn {
    /// From the `information_schema` data type.
    pub fn new(name: &str, data_type: &str) -> Self {
        let cast = match data_type.to_ascii_lowercase().as_str() {
            // `CAST(? AS DECIMAL(65,0))` of a text value saturates at the signed
            // maximum; a decimal is exact for every integer width.
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" => {
                Some("DECIMAL(65,0)".to_string())
            }
            "decimal" | "numeric" => Some("DECIMAL(65,30)".to_string()),
            _ => None,
        };
        Self {
            name: name.to_string(),
            cast,
        }
    }
}

/// SELECT-list expression giving the row id of alias `t`.
pub(crate) fn id_expr(key: &[KeyColumn]) -> String {
    let cols: Vec<String> = key
        .iter()
        .map(|k| format!("CAST(t.{} AS CHAR)", quote(&k.name)))
        .collect();
    format!("CAST(JSON_ARRAY({}) AS CHAR)", cols.join(", "))
}

/// The key values in a row id, or `None` when it is not one of ours.
pub(crate) fn values(id: &str, key: &[KeyColumn]) -> Option<Vec<Option<String>>> {
    let Json::Arr(items) = parse_json(id).ok()? else {
        return None;
    };
    if items.len() != key.len() || key.is_empty() {
        return None;
    }
    items
        .into_iter()
        .map(|j| match j {
            Json::Str(s) => Some(Some(s)),
            Json::Null => Some(None),
            _ => None,
        })
        .collect()
}

/// `WHERE` condition for the row, with one `?` per non-NULL key value,
/// and the values to bind in that order.
pub(crate) fn matches(id: &str, key: &[KeyColumn]) -> Option<(String, Vec<String>)> {
    let vals = values(id, key)?;
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    for (k, v) in key.iter().zip(vals) {
        match v {
            None => parts.push(format!("{} IS NULL", quote(&k.name))),
            Some(v) => {
                parts.push(match &k.cast {
                    Some(cast) => format!("{} = CAST(? AS {cast})", quote(&k.name)),
                    None => format!("{} = ?", quote(&k.name)),
                });
                binds.push(v);
            }
        }
    }
    Some((parts.join(" AND "), binds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Vec<KeyColumn> {
        vec![
            KeyColumn::new("id", "bigint"),
            KeyColumn::new("co`de", "varchar"),
        ]
    }

    #[test]
    fn keys_select_and_match_by_value() {
        assert_eq!(
            id_expr(&key()),
            "CAST(JSON_ARRAY(CAST(t.`id` AS CHAR), CAST(t.`co``de` AS CHAR)) AS CHAR)"
        );
        let (sql, binds) = matches("[\"12\",\"a\"]", &key()).unwrap();
        assert_eq!(sql, "`id` = CAST(? AS DECIMAL(65,0)) AND `co``de` = ?");
        assert_eq!(binds, ["12", "a"]);
    }

    #[test]
    fn null_key_parts_and_foreign_ids_are_handled() {
        let (sql, binds) = matches("[\"1\",null]", &key()).unwrap();
        assert_eq!(sql, "`id` = CAST(? AS DECIMAL(65,0)) AND `co``de` IS NULL");
        assert_eq!(binds, ["1"]);
        assert!(matches("[\"1\"]", &key()).is_none());
        assert!(matches("AAAB", &key()).is_none());
        assert!(matches("[\"1\"]", &[]).is_none());
    }
}
