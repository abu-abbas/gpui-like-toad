//! Oracle 12c+ SQL.

use super::literal::{
    base_type, bool_digit, hex_upper, looks_numeric, no_nul, quote_text, split_timestamp, type_args,
};
use super::{CanonType, Capabilities, DbKind, Dialect};
use crate::filter::TablePage;
use crate::model::{ColumnDef, Value};

pub struct OracleDialect;

/// Characters a ROWID may hold; checked before it is written into SQL.
fn valid_rowid(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 4000
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '*' | '.' | '-' | '_'))
}

impl Dialect for OracleDialect {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schemas: true,
            materialized_views: true,
            password: true,
            network: true,
            row_id: true,
            sysdba: true,
            transactional_ddl: false,
            row_edit: true,
            explain_text: true,
            explain_analyze: false,
            truncate: true,
            special_tables: true,
            programs: true,
            sequences: true,
            recycle_bin: true,
        }
    }

    fn default_port(&self) -> Option<u16> {
        Some(1521)
    }

    fn database_label(&self) -> &'static str {
        "Service name"
    }

    fn new_table_types(&self) -> (&'static str, &'static str) {
        ("NUMBER", "VARCHAR2(100)")
    }

    fn row_id_expr(&self) -> &'static str {
        "t.ROWID"
    }

    fn row_id_match(&self, bind: usize) -> String {
        format!("ROWID = CHARTOROWID(:{bind})")
    }

    fn keyset_after(&self, key: &str, after: &Value) -> Result<String, String> {
        match after {
            Value::Text(t) if valid_rowid(t) => Ok(format!("{key} > CHARTOROWID('{t}')")),
            _ => Err("The row key of the source is not usable for paging.".to_string()),
        }
    }

    fn push_page(&self, sql: &mut String, page: TablePage) {
        if page.offset == 0 {
            sql.push_str(&format!(" FETCH FIRST {} ROWS ONLY", page.limit));
        } else {
            sql.push_str(&format!(
                " OFFSET {} ROWS FETCH NEXT {} ROWS ONLY",
                page.offset, page.limit
            ));
        }
    }

    fn is_auto_generated(&self, column: &ColumnDef) -> bool {
        // Identity columns and sequence defaults are both stored as a
        // default expression ending in `.NEXTVAL`.
        column
            .default
            .as_deref()
            .is_some_and(|expr| expr.to_ascii_uppercase().contains(".NEXTVAL"))
    }

    fn canonical_type(&self, native: &str) -> CanonType {
        let base = base_type(native);
        let args = type_args(native);
        let scale_zero = {
            let inner = native
                .split_once('(')
                .map(|(_, r)| r.trim_end_matches(')').replace(' ', ""))
                .unwrap_or_default();
            !inner.is_empty() && (!inner.contains(',') || inner.ends_with(",0"))
        };
        match base.as_str() {
            "NUMBER" | "DECIMAL" | "NUMERIC" if scale_zero => CanonType::Integer {
                digits: args.first().copied(),
            },
            "INTEGER" | "INT" | "SMALLINT" => CanonType::Integer { digits: None },
            "NUMBER" | "DECIMAL" | "NUMERIC" => CanonType::Decimal {
                precision: args.first().copied(),
                scale: args.get(1).copied(),
            },
            "FLOAT" | "BINARY_FLOAT" | "BINARY_DOUBLE" | "REAL" | "DOUBLE PRECISION" => {
                CanonType::Double
            }
            "BLOB" | "RAW" | "LONG RAW" => CanonType::Blob,
            "CLOB" | "NCLOB" | "LONG" => CanonType::Clob,
            "VARCHAR2" | "NVARCHAR2" | "VARCHAR" | "CHAR" | "NCHAR" => CanonType::Text {
                len: args.first().copied(),
            },
            "DATE" => CanonType::Timestamp,
            t if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") => CanonType::TimestampTz,
            t if t.starts_with("TIMESTAMP") => CanonType::Timestamp,
            _ => CanonType::Text { len: None },
        }
    }

    fn native_type(&self, canonical: &CanonType) -> String {
        match canonical {
            CanonType::Integer { digits } => format!("NUMBER({})", digits.unwrap_or(19).min(38)),
            CanonType::Decimal {
                precision: Some(p),
                scale: Some(s),
            } => format!("NUMBER({},{s})", (*p).min(38)),
            CanonType::Decimal {
                precision: Some(p), ..
            } => format!("NUMBER({})", (*p).min(38)),
            CanonType::Decimal { .. } => "NUMBER".to_string(),
            CanonType::Double => "BINARY_DOUBLE".to_string(),
            CanonType::Text { len: Some(n) } if *n <= 4000 => format!("VARCHAR2({n})"),
            CanonType::Text { len: Some(_) } | CanonType::Clob | CanonType::Json => {
                "CLOB".to_string()
            }
            CanonType::Text { len: None } => "VARCHAR2(4000)".to_string(),
            CanonType::Binary => "RAW(2000)".to_string(),
            CanonType::Blob => "BLOB".to_string(),
            CanonType::Date => "DATE".to_string(),
            CanonType::Timestamp => "TIMESTAMP(6)".to_string(),
            CanonType::TimestampTz => "TIMESTAMP(6) WITH TIME ZONE".to_string(),
            CanonType::Time => "VARCHAR2(32)".to_string(),
            CanonType::Boolean => "NUMBER(1)".to_string(),
        }
    }

    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String> {
        if bytes.len() <= 2000 {
            Ok(format!("HEXTORAW('{}')", hex_upper(bytes)))
        } else {
            Err("a binary value over 2000 bytes cannot be sent to Oracle".to_string())
        }
    }

    fn text_literal(&self, column_type: &str, s: &str) -> Result<String, String> {
        let base = base_type(column_type);
        no_nul(s)?;
        let numeric = matches!(
            base.as_str(),
            "NUMBER"
                | "FLOAT"
                | "BINARY_FLOAT"
                | "BINARY_DOUBLE"
                | "INTEGER"
                | "INT"
                | "SMALLINT"
                | "DECIMAL"
                | "NUMERIC"
                | "REAL"
        );
        if numeric && looks_numeric(s) {
            return Ok(s.trim().to_string());
        }
        if numeric {
            if let Some(digit) = bool_digit(s) {
                return Ok(digit.to_string());
            }
        }
        if base == "DATE" || (base.starts_with("TIMESTAMP") && !base.contains("TIME ZONE")) {
            if let Some((main, frac)) = split_timestamp(s) {
                let date_only = main.len() == 10;
                let text = main.replacen('T', " ", 1);
                return Ok(match (base == "DATE", date_only, frac) {
                    (true, true, _) => format!("TO_DATE('{text}', 'YYYY-MM-DD')"),
                    (true, false, _) => format!("TO_DATE('{text}', 'YYYY-MM-DD HH24:MI:SS')"),
                    (false, true, _) => format!("TO_TIMESTAMP('{text}', 'YYYY-MM-DD')"),
                    (false, false, None) => {
                        format!("TO_TIMESTAMP('{text}', 'YYYY-MM-DD HH24:MI:SS')")
                    }
                    (false, false, Some(f)) => {
                        format!("TO_TIMESTAMP('{text}.{f}', 'YYYY-MM-DD HH24:MI:SS.FF')")
                    }
                });
            }
        }
        // A plain literal is limited to 4000 bytes: longer text goes in as
        // TO_CLOB pieces.
        if s.len() <= 4000 {
            return Ok(quote_text(s));
        }
        let mut parts = Vec::new();
        let mut piece = String::new();
        for c in s.chars() {
            if piece.len() + c.len_utf8() > 2000 {
                parts.push(format!("TO_CLOB({})", quote_text(&piece)));
                piece.clear();
            }
            piece.push(c);
        }
        parts.push(format!("TO_CLOB({})", quote_text(&piece)));
        Ok(parts.join(" || "))
    }
}
