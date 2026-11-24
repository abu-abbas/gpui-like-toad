//! PostgreSQL SQL.

use super::literal::{base_type, hex_upper, looks_numeric, no_nul, quote_text, type_args};
use super::{numeric_column, BindStyle, CanonType, Capabilities, DbKind, Dialect};
use crate::model::{ColumnDef, Value};

pub struct PostgresDialect;

/// `(block,offset)`, the text form of a `ctid`.
fn valid_ctid(text: &str) -> bool {
    let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
        return false;
    };
    let Some((block, offset)) = inner.split_once(',') else {
        return false;
    };
    let number = |s: &str| !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit());
    number(block) && number(offset)
}

impl Dialect for PostgresDialect {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schemas: true,
            databases: false,
            materialized_views: true,
            password: true,
            network: true,
            row_id: true,
            sysdba: false,
            transactional_ddl: true,
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
        ("bigint", "varchar(100)")
    }

    fn drop_object(
        &self,
        noun: &str,
        owner: &str,
        name: &str,
        parent: Option<&str>,
    ) -> Option<String> {
        if noun == "TRIGGER" {
            // A trigger is named per table.
            return Some(format!(
                "DROP TRIGGER {} ON {}",
                self.quote_ident(name),
                self.table_ref(owner, parent?)
            ));
        }
        Some(format!("DROP {noun} {}", self.table_ref(owner, name)))
    }

    fn bind_style(&self) -> BindStyle {
        BindStyle::Dollar
    }

    fn default_port(&self) -> Option<u16> {
        Some(5432)
    }

    /// The `ctid` as text: `(block,offset)`. It changes when a row is
    /// updated, so it identifies a row only until the next write to it.
    fn row_id_expr(&self) -> &'static str {
        "t.ctid::text"
    }

    fn row_id_sort_expr(&self) -> &'static str {
        "t.ctid"
    }

    fn row_id_match(&self, bind: usize) -> String {
        format!("ctid = CAST(CAST(:{bind} AS text) AS tid)")
    }

    fn keyset_after(&self, key: &str, after: &Value) -> Result<String, String> {
        match after {
            Value::Text(t) if valid_ctid(t) => Ok(format!("{key} > '{t}'::tid")),
            _ => Err("The row key of the source is not usable for paging.".to_string()),
        }
    }

    fn is_auto_generated(&self, column: &ColumnDef) -> bool {
        column.default.as_deref().is_some_and(|expr| {
            let lower = expr.trim_start().to_ascii_lowercase();
            lower.starts_with("nextval(") || lower.contains("as identity")
        })
    }

    fn date_literal(&self, iso: &str) -> String {
        format!("DATE '{iso}'")
    }

    fn index_refs(&self, schema: &str, index: &str, table: &str) -> (String, String) {
        // The index lives in its table's schema and takes no qualifier.
        (self.quote_ident(index), self.table_ref(schema, table))
    }

    fn canonical_type(&self, native: &str) -> CanonType {
        let base = base_type(native);
        let args = type_args(native);
        match base.as_str() {
            "SMALLINT" | "INT2" => CanonType::Integer { digits: Some(5) },
            "INTEGER" | "INT" | "INT4" | "SERIAL" => CanonType::Integer { digits: Some(10) },
            "BIGINT" | "INT8" | "BIGSERIAL" => CanonType::Integer { digits: Some(19) },
            "NUMERIC" | "DECIMAL" => CanonType::Decimal {
                precision: args.first().copied(),
                scale: args.get(1).copied(),
            },
            "REAL" | "FLOAT4" | "DOUBLE PRECISION" | "FLOAT8" | "FLOAT" => CanonType::Double,
            "BYTEA" => CanonType::Blob,
            "TEXT" => CanonType::Clob,
            "CHARACTER VARYING" | "VARCHAR" | "CHARACTER" | "CHAR" | "BPCHAR" => CanonType::Text {
                len: args.first().copied(),
            },
            "UUID" => CanonType::Text { len: Some(36) },
            "DATE" => CanonType::Date,
            "BOOLEAN" | "BOOL" => CanonType::Boolean,
            "JSON" | "JSONB" => CanonType::Json,
            t if t.starts_with("TIMESTAMP")
                && (t.contains("WITH TIME ZONE") || t == "TIMESTAMPTZ") =>
            {
                CanonType::TimestampTz
            }
            t if t.starts_with("TIMESTAMP") => CanonType::Timestamp,
            t if t.starts_with("TIME") => CanonType::Time,
            _ => CanonType::Text { len: None },
        }
    }

    fn native_type(&self, canonical: &CanonType) -> String {
        match canonical {
            CanonType::Integer { digits } => match digits {
                Some(d) if *d <= 4 => "SMALLINT".to_string(),
                Some(d) if *d <= 9 => "INTEGER".to_string(),
                Some(d) if *d <= 18 => "BIGINT".to_string(),
                Some(d) => format!("NUMERIC({})", (*d).min(1000)),
                None => "BIGINT".to_string(),
            },
            CanonType::Decimal {
                precision: Some(p),
                scale: Some(s),
            } => format!("NUMERIC({},{s})", (*p).min(1000)),
            CanonType::Decimal {
                precision: Some(p), ..
            } => format!("NUMERIC({})", (*p).min(1000)),
            CanonType::Decimal { .. } => "NUMERIC".to_string(),
            CanonType::Double => "DOUBLE PRECISION".to_string(),
            CanonType::Text { len: Some(n) } => format!("VARCHAR({n})"),
            CanonType::Text { len: None } | CanonType::Clob => "TEXT".to_string(),
            CanonType::Binary | CanonType::Blob => "BYTEA".to_string(),
            CanonType::Date => "DATE".to_string(),
            CanonType::Timestamp => "TIMESTAMP".to_string(),
            CanonType::TimestampTz => "TIMESTAMP WITH TIME ZONE".to_string(),
            CanonType::Time => "TIME".to_string(),
            CanonType::Boolean => "BOOLEAN".to_string(),
            CanonType::Json => "JSONB".to_string(),
        }
    }

    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String> {
        Ok(format!("'\\x{}'::bytea", hex_upper(bytes)))
    }

    fn text_literal(&self, column_type: &str, s: &str) -> Result<String, String> {
        no_nul(s)?;
        if numeric_column(&base_type(column_type)) && looks_numeric(s) {
            Ok(s.trim().to_string())
        } else {
            // Dates, booleans, JSON and the rest are read by the server
            // from a quoted literal, as the column's own type.
            Ok(quote_text(s))
        }
    }
}
