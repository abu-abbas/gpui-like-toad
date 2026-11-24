//! MySQL and MariaDB SQL. A "schema" is a database.

use super::literal::{base_type, bool_digit, hex_upper, looks_numeric, no_nul, type_args};
use super::{numeric_column, BindStyle, CanonType, Capabilities, DbKind, Dialect};
use crate::model::{ColumnDef, Value};

pub struct MysqlDialect;

impl Dialect for MysqlDialect {
    fn kind(&self) -> DbKind {
        DbKind::Mysql
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schemas: true,
            databases: false,
            materialized_views: false,
            password: true,
            network: true,
            // No physical row id: rows are found by primary key, which the
            // driver reads per table.
            row_id: false,
            sysdba: false,
            transactional_ddl: false,
            row_edit: true,
            explain_text: true,
            truncate: true,
            special_tables: false,
            programs: false,
            sequences: false,
            recycle_bin: false,
        }
    }

    fn new_table_types(&self) -> (&'static str, &'static str) {
        ("BIGINT", "VARCHAR(100)")
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn bind_style(&self) -> BindStyle {
        BindStyle::Question
    }

    fn default_port(&self) -> Option<u16> {
        Some(3306)
    }

    fn row_id_expr(&self) -> &'static str {
        // Never selected: `capabilities().row_id` is false.
        "NULL"
    }

    fn row_id_match(&self, _bind: usize) -> String {
        "1 = 0".to_string()
    }

    fn keyset_after(&self, _key: &str, _after: &Value) -> Result<String, String> {
        Err("MySQL has no row key to page by.".to_string())
    }

    fn is_auto_generated(&self, column: &ColumnDef) -> bool {
        column
            .default
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case("auto_increment"))
    }

    fn default_clause(&self, expr: &str) -> String {
        // An expression default needs its own parentheses.
        let simple = expr.trim().starts_with('\'')
            || expr.trim().chars().all(|c| c.is_ascii_digit() || c == '.')
            || expr
                .trim()
                .to_ascii_uppercase()
                .starts_with("CURRENT_TIMESTAMP");
        if simple {
            format!(" DEFAULT {expr}")
        } else {
            format!(" DEFAULT ({expr})")
        }
    }

    fn rename_table(&self, owner: &str, old: &str, new: &str) -> String {
        format!(
            "RENAME TABLE {} TO {}",
            self.table_ref(owner, old),
            self.table_ref(owner, new)
        )
    }

    fn index_refs(&self, schema: &str, index: &str, table: &str) -> (String, String) {
        (self.quote_ident(index), self.table_ref(schema, table))
    }

    fn key_type(&self, native: &str) -> Option<String> {
        match base_type(native).as_str() {
            "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => Some("VARCHAR(255)".to_string()),
            "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => Some("VARBINARY(255)".to_string()),
            _ => None,
        }
    }

    fn date_literal(&self, iso: &str) -> String {
        format!("DATE '{iso}'")
    }

    fn canonical_type(&self, native: &str) -> CanonType {
        let base = base_type(native);
        let args = type_args(native);
        let t = native.trim().to_uppercase();
        match base.as_str() {
            "BOOL" | "BOOLEAN" => CanonType::Boolean,
            "TINYINT" if args == [1] => CanonType::Boolean,
            "TINYINT" => CanonType::Integer { digits: Some(3) },
            "SMALLINT" | "YEAR" => CanonType::Integer { digits: Some(5) },
            "MEDIUMINT" => CanonType::Integer { digits: Some(8) },
            "INT" | "INTEGER" => CanonType::Integer { digits: Some(10) },
            "BIGINT" => CanonType::Integer { digits: Some(20) },
            "DECIMAL" | "NUMERIC" | "FIXED" => CanonType::Decimal {
                precision: args.first().copied(),
                scale: args.get(1).copied(),
            },
            "FLOAT" | "DOUBLE" | "REAL" | "DOUBLE PRECISION" => CanonType::Double,
            "VARCHAR" | "CHAR" => CanonType::Text {
                len: args.first().copied(),
            },
            "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => CanonType::Clob,
            "BINARY" | "VARBINARY" => CanonType::Binary,
            "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => CanonType::Blob,
            "DATE" => CanonType::Date,
            "DATETIME" | "TIMESTAMP" => CanonType::Timestamp,
            "TIME" => CanonType::Time,
            "JSON" => CanonType::Json,
            _ if t.starts_with("ENUM") || t.starts_with("SET") => CanonType::Text { len: None },
            _ => CanonType::Text { len: None },
        }
    }

    fn native_type(&self, canonical: &CanonType) -> String {
        match canonical {
            CanonType::Integer { digits } => match digits {
                Some(d) if *d <= 9 => "INT".to_string(),
                Some(d) if *d <= 18 => "BIGINT".to_string(),
                Some(d) => format!("DECIMAL({},0)", (*d).min(65)),
                None => "BIGINT".to_string(),
            },
            CanonType::Decimal {
                precision: Some(p),
                scale: Some(s),
            } => format!("DECIMAL({},{})", (*p).min(65), (*s).min(30)),
            CanonType::Decimal {
                precision: Some(p), ..
            } => format!("DECIMAL({})", (*p).min(65)),
            CanonType::Decimal { .. } => "DECIMAL(38,10)".to_string(),
            CanonType::Double => "DOUBLE".to_string(),
            CanonType::Text { len: Some(n) } if *n <= 16000 => format!("VARCHAR({n})"),
            CanonType::Text { .. } | CanonType::Clob => "LONGTEXT".to_string(),
            CanonType::Binary | CanonType::Blob => "LONGBLOB".to_string(),
            CanonType::Date => "DATE".to_string(),
            CanonType::Timestamp | CanonType::TimestampTz => "DATETIME(6)".to_string(),
            CanonType::Time => "TIME".to_string(),
            CanonType::Boolean => "TINYINT(1)".to_string(),
            CanonType::Json => "JSON".to_string(),
        }
    }

    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String> {
        Ok(format!("X'{}'", hex_upper(bytes)))
    }

    fn text_literal(&self, column_type: &str, s: &str) -> Result<String, String> {
        no_nul(s)?;
        if numeric_column(&base_type(column_type)) {
            if looks_numeric(s) {
                return Ok(s.trim().to_string());
            }
            if let Some(digit) = bool_digit(s) {
                return Ok(digit.to_string());
            }
        }
        // Backslash is an escape character in MySQL strings by default.
        Ok(format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")))
    }
}
