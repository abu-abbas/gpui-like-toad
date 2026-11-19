//! SQLite SQL. No driver uses it yet; it is here so the shared builders
//! are tested against a second dialect from the start.

use super::{Capabilities, DbKind, Dialect};
use crate::filter::TablePage;

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
            sysdba: false,
            explain_text: false,
        }
    }

    fn row_id_expr(&self) -> &'static str {
        "t.rowid"
    }

    fn row_id_match(&self, bind: usize) -> String {
        format!("rowid = :{bind}")
    }

    fn push_page(&self, sql: &mut String, page: TablePage) {
        sql.push_str(&format!(" LIMIT {}", page.limit));
        if page.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", page.offset));
        }
    }
}
