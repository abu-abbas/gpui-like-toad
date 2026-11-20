//! Per-database SQL differences, kept out of the rest of the crate.
//!
//! Everything that builds SQL for the data tab (paging, finding a row by its
//! row id, quoting a table name) asks a [`Dialect`] instead of writing
//! Oracle syntax inline. Each database gets its own small file here; nothing
//! else in the crate needs to know which database it is talking to.
//!
//! Bind variables are always positional `:1`, `:2`, ... in every dialect:
//! Oracle reads them natively, and SQLite accepts `:1` as a named parameter
//! that a driver binds by that name. That keeps the filter builder and
//! [`crate::filter::BoundQuery`] previews dialect-free.

mod oracle;
mod sqlite;

pub use oracle::OracleDialect;
pub use sqlite::SqliteDialect;

use crate::filter::TablePage;

/// Which database engine a connection profile points at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DbKind {
    #[default]
    Oracle,
    Sqlite,
}

impl DbKind {
    pub const ALL: [Self; 2] = [Self::Oracle, Self::Sqlite];

    /// Name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Oracle => "Oracle",
            Self::Sqlite => "SQLite",
        }
    }

    /// Spelling stored in `profiles.json`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Oracle => "oracle",
            Self::Sqlite => "sqlite",
        }
    }

    /// Parse a stored key. Unknown or missing keys mean Oracle, so profile
    /// files written before this field existed keep working.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|kind| kind.key().eq_ignore_ascii_case(key.trim()))
            .unwrap_or_default()
    }

    pub fn dialect(self) -> &'static dyn Dialect {
        match self {
            Self::Oracle => &OracleDialect,
            Self::Sqlite => &SqliteDialect,
        }
    }

    pub fn capabilities(self) -> Capabilities {
        self.dialect().capabilities()
    }
}

/// Features that exist on some databases only. The UI hides what a
/// connection's database does not have instead of showing menus that would
/// fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// More than one browsable schema (Oracle users). SQLite has `main`
    /// plus attached databases, so it hides the schema dropdown.
    pub schemas: bool,
    /// Materialized views, and "Refresh Materialized View".
    pub materialized_views: bool,
    /// Connecting with a username and password (and saving it in the
    /// keychain). SQLite opens a file.
    pub password: bool,
    /// "Connect as SYSDBA".
    pub sysdba: bool,
    /// Explain Plan as formatted text (DBMS_XPLAN), next to the plan tree.
    pub explain_text: bool,
    /// `TRUNCATE TABLE`.
    pub truncate: bool,
    /// External and index-organized tables (New Table types).
    pub special_tables: bool,
    /// Stored programs: Compile, Execute Function.
    pub programs: bool,
    /// Sequences: Reset Sequence.
    pub sequences: bool,
}

/// SQL text that differs per database. Identifiers are double-quoted in
/// every database supported so far, so quoting has a shared default.
pub trait Dialect: Sync {
    fn kind(&self) -> DbKind;

    fn capabilities(&self) -> Capabilities;

    /// Quote one identifier, doubling embedded quotes.
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    /// `"OWNER"."TABLE"`.
    fn table_ref(&self, owner: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(owner), self.quote_ident(table))
    }

    /// Expression selecting the row id of table alias `t`, e.g. `t.ROWID`.
    fn row_id_expr(&self) -> &'static str;

    /// `WHERE` condition matching one row by the row id in bind `:n`.
    fn row_id_match(&self, bind: usize) -> String;

    /// Append the row window for `page` to a `SELECT`.
    fn push_page(&self, sql: &mut String, page: TablePage);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_keys_round_trip_and_default_to_oracle() {
        for kind in DbKind::ALL {
            assert_eq!(DbKind::from_key(kind.key()), kind);
            assert_eq!(kind.dialect().kind(), kind);
        }
        assert_eq!(DbKind::from_key(""), DbKind::Oracle);
        assert_eq!(DbKind::from_key("SQLite"), DbKind::Sqlite);
        assert_eq!(DbKind::from_key("mystery"), DbKind::Oracle);
    }

    #[test]
    fn shared_quoting_doubles_quotes() {
        assert_eq!(OracleDialect.table_ref("HR", "A\"B"), "\"HR\".\"A\"\"B\"");
        assert_eq!(SqliteDialect.table_ref("main", "t"), "\"main\".\"t\"");
    }
}
