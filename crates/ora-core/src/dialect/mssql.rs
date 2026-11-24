//! SQL Server SQL. One session can read every database of the server by
//! naming it, so identifiers are `[database].[schema].[object]` and the
//! data tab binds each tab to the database it was opened in. The driver is
//! the `kilat-ext-mssql` extension; this file is the part the app and the
//! extension share, including the paging syntax (`OFFSET ... FETCH` needs an
//! `ORDER BY`, SQL Server 2012 and later).

use super::literal::{base_type, bool_digit, hex_upper, looks_numeric, no_nul, type_args};
use super::{numeric_column, BindStyle, CanonType, Capabilities, DbKind, Dialect};
use crate::filter::TablePage;
use crate::model::Value;

pub struct MssqlDialect;

impl Dialect for MssqlDialect {
    fn kind(&self) -> DbKind {
        DbKind::Mssql
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // The dropdown lists databases; objects are grouped by schema.
            schemas: true,
            databases: true,
            materialized_views: false,
            password: true,
            network: true,
            // Rows have no physical id the data tab could edit by, so the
            // grid is read-only.
            row_id: false,
            sysdba: false,
            transactional_ddl: true,
            row_edit: false,
            explain_text: false,
            explain_analyze: false,
            truncate: false,
            special_tables: false,
            programs: false,
            sequences: false,
            recycle_bin: false,
        }
    }

    fn new_table_types(&self) -> (&'static str, &'static str) {
        ("BIGINT", "NVARCHAR(100)")
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("[{}]", ident.replace(']', "]]"))
    }

    fn table_ref_in(&self, database: Option<&str>, owner: &str, table: &str) -> String {
        match database.filter(|d| !d.is_empty()) {
            Some(db) => format!(
                "{}.{}.{}",
                self.quote_ident(db),
                self.quote_ident(owner),
                self.quote_ident(table)
            ),
            None => self.table_ref(owner, table),
        }
    }

    /// Without a key to sort by there is no stable order; `(SELECT NULL)`
    /// keeps the statement valid, and rows with equal sort values may move
    /// between pages.
    fn push_page_ordered(&self, sql: &mut String, page: TablePage, order_by: &[String]) {
        if order_by.is_empty() {
            sql.push_str(" ORDER BY (SELECT NULL)");
        } else {
            let columns: Vec<String> = order_by.iter().map(|c| self.quote_ident(c)).collect();
            sql.push_str(&format!(" ORDER BY {}", columns.join(", ")));
        }
        sql.push_str(&format!(
            " OFFSET {} ROWS FETCH NEXT {} ROWS ONLY",
            page.offset, page.limit
        ));
    }

    fn push_page(&self, sql: &mut String, page: TablePage) {
        self.push_page_ordered(sql, page, &[]);
    }

    fn bind_style(&self) -> BindStyle {
        BindStyle::AtP
    }

    fn default_port(&self) -> Option<u16> {
        Some(1433)
    }

    fn row_id_expr(&self) -> &'static str {
        // Never selected: `capabilities().row_id` is false.
        "NULL"
    }

    fn row_id_match(&self, _bind: usize) -> String {
        "1 = 0".to_string()
    }

    fn keyset_after(&self, _key: &str, _after: &Value) -> Result<String, String> {
        Err("SQL Server has no row key to page by.".to_string())
    }

    fn canonical_type(&self, native: &str) -> CanonType {
        let base = base_type(native);
        let args = type_args(native);
        match base.as_str() {
            "BIT" => CanonType::Boolean,
            "TINYINT" => CanonType::Integer { digits: Some(3) },
            "SMALLINT" => CanonType::Integer { digits: Some(5) },
            "INT" | "INTEGER" => CanonType::Integer { digits: Some(10) },
            "BIGINT" => CanonType::Integer { digits: Some(19) },
            "DECIMAL" | "NUMERIC" => CanonType::Decimal {
                precision: args.first().copied(),
                scale: args.get(1).copied(),
            },
            "MONEY" => CanonType::Decimal {
                precision: Some(19),
                scale: Some(4),
            },
            "SMALLMONEY" => CanonType::Decimal {
                precision: Some(10),
                scale: Some(4),
            },
            "FLOAT" | "REAL" => CanonType::Double,
            "CHAR" | "VARCHAR" | "NCHAR" | "NVARCHAR" => match args.first() {
                Some(n) if native.to_uppercase().contains("MAX") || *n == 0 => CanonType::Clob,
                Some(n) => CanonType::Text { len: Some(*n) },
                None if native.to_uppercase().contains("MAX") => CanonType::Clob,
                None => CanonType::Text { len: Some(1) },
            },
            "TEXT" | "NTEXT" | "XML" => CanonType::Clob,
            "BINARY" | "VARBINARY" if native.to_uppercase().contains("MAX") => CanonType::Blob,
            "BINARY" | "VARBINARY" => CanonType::Binary,
            "IMAGE" => CanonType::Blob,
            "DATE" => CanonType::Date,
            "DATETIME" | "DATETIME2" | "SMALLDATETIME" => CanonType::Timestamp,
            "DATETIMEOFFSET" => CanonType::TimestampTz,
            "TIME" => CanonType::Time,
            "UNIQUEIDENTIFIER" => CanonType::Text { len: Some(36) },
            _ => CanonType::Text { len: None },
        }
    }

    fn native_type(&self, canonical: &CanonType) -> String {
        match canonical {
            CanonType::Integer { digits } => match digits {
                Some(d) if *d <= 3 => "TINYINT".to_string(),
                Some(d) if *d <= 5 => "SMALLINT".to_string(),
                Some(d) if *d <= 10 => "INT".to_string(),
                Some(d) if *d <= 19 => "BIGINT".to_string(),
                Some(d) => format!("DECIMAL({},0)", (*d).min(38)),
                None => "BIGINT".to_string(),
            },
            CanonType::Decimal {
                precision: Some(p),
                scale: Some(s),
            } => format!("DECIMAL({},{})", (*p).min(38), (*s).min(38)),
            CanonType::Decimal {
                precision: Some(p), ..
            } => format!("DECIMAL({})", (*p).min(38)),
            CanonType::Decimal { .. } => "DECIMAL(38,10)".to_string(),
            CanonType::Double => "FLOAT".to_string(),
            CanonType::Text { len: Some(n) } if *n <= 4000 => format!("NVARCHAR({n})"),
            CanonType::Text { .. } | CanonType::Clob => "NVARCHAR(MAX)".to_string(),
            CanonType::Binary | CanonType::Blob => "VARBINARY(MAX)".to_string(),
            CanonType::Date => "DATE".to_string(),
            CanonType::Timestamp => "DATETIME2".to_string(),
            CanonType::TimestampTz => "DATETIMEOFFSET".to_string(),
            CanonType::Time => "TIME".to_string(),
            CanonType::Boolean => "BIT".to_string(),
            CanonType::Json => "NVARCHAR(MAX)".to_string(),
        }
    }

    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String> {
        Ok(format!("0x{}", hex_upper(bytes)))
    }

    fn text_literal(&self, column_type: &str, text: &str) -> Result<String, String> {
        no_nul(text)?;
        if numeric_column(&base_type(column_type)) {
            if looks_numeric(text) {
                return Ok(text.trim().to_string());
            }
            if let Some(digit) = bool_digit(text) {
                return Ok(digit.to_string());
            }
        }
        Ok(format!("N'{}'", text.replace('\'', "''")))
    }
}
