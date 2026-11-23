//! SQLite SQL. The driver is the `kilat-ext-sqlite` extension; this file is
//! the part of it the app and the extension share.

use super::literal::{base_type, hex_upper, looks_numeric, no_nul, quote_text};
use super::{numeric_column, CanonType, Capabilities, DbKind, Dialect};
use crate::model::Value;

pub struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schemas: false,
            materialized_views: false,
            password: false,
            network: false,
            row_id: true,
            sysdba: false,
            explain_text: false,
            truncate: false,
            special_tables: false,
            programs: false,
            sequences: false,
            recycle_bin: false,
        }
    }

    fn row_id_expr(&self) -> &'static str {
        "t.rowid"
    }

    fn row_id_match(&self, bind: usize) -> String {
        format!("rowid = :{bind}")
    }

    fn keyset_after(&self, key: &str, after: &Value) -> Result<String, String> {
        match after {
            Value::Integer(n) => Ok(format!("{key} > {n}")),
            _ => Err("The row key of the source is not usable for paging.".to_string()),
        }
    }

    fn default_clause(&self, expr: &str) -> String {
        format!(" DEFAULT ({expr})")
    }

    fn foreign_keys_inline(&self) -> bool {
        true
    }

    fn foreign_key_parent(&self, _schema: &str, parent: &str) -> String {
        self.quote_ident(parent)
    }

    fn index_refs(&self, schema: &str, index: &str, table: &str) -> (String, String) {
        (self.table_ref(schema, index), self.quote_ident(table))
    }

    fn date_literal(&self, iso: &str) -> String {
        format!("'{iso}'")
    }

    fn canonical_type(&self, native: &str) -> CanonType {
        let t = native.trim().to_uppercase();
        let has = |s: &str| t.contains(s);
        if has("DATE") || has("TIME") {
            // SQLite keeps dates as text.
            CanonType::Text { len: Some(64) }
        } else if has("BOOL") {
            CanonType::Boolean
        } else if has("INT") {
            CanonType::Integer { digits: None }
        } else if has("CHAR") || has("CLOB") || has("TEXT") || t.is_empty() {
            CanonType::Text { len: None }
        } else if has("BLOB") {
            CanonType::Blob
        } else if has("REAL") || has("FLOA") || has("DOUB") {
            CanonType::Double
        } else {
            CanonType::Decimal {
                precision: None,
                scale: None,
            }
        }
    }

    fn native_type(&self, canonical: &CanonType) -> String {
        match canonical {
            CanonType::Integer { .. } | CanonType::Boolean => "INTEGER",
            CanonType::Decimal { .. } => "NUMERIC",
            CanonType::Double => "REAL",
            CanonType::Binary | CanonType::Blob => "BLOB",
            CanonType::Text { .. }
            | CanonType::Clob
            | CanonType::Date
            | CanonType::Timestamp
            | CanonType::TimestampTz
            | CanonType::Time
            | CanonType::Json => "TEXT",
        }
        .to_string()
    }

    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String> {
        Ok(format!("X'{}'", hex_upper(bytes)))
    }

    fn text_literal(&self, column_type: &str, s: &str) -> Result<String, String> {
        no_nul(s)?;
        if numeric_column(&base_type(column_type)) && looks_numeric(s) {
            Ok(s.trim().to_string())
        } else {
            Ok(quote_text(s))
        }
    }
}
