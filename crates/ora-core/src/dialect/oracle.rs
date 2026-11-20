//! Oracle 12c+ SQL.

use super::{Capabilities, DbKind, Dialect};
use crate::filter::TablePage;

pub struct OracleDialect;

impl Dialect for OracleDialect {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schemas: true,
            materialized_views: true,
            password: true,
            sysdba: true,
            explain_text: true,
            truncate: true,
            special_tables: true,
            programs: true,
            sequences: true,
        }
    }

    fn row_id_expr(&self) -> &'static str {
        "t.ROWID"
    }

    fn row_id_match(&self, bind: usize) -> String {
        format!("ROWID = CHARTOROWID(:{bind})")
    }

    /// Offset and limit are `usize`s written as literals, so they cannot
    /// carry SQL.
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
}
