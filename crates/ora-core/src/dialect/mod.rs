//! Per-database SQL differences, kept out of the rest of the crate.
//!
//! Everything that builds SQL for the data tab (paging, finding a row by its
//! row id, quoting a table name) asks a [`Dialect`] instead of writing
//! Oracle syntax inline. Each database gets its own small file here; nothing
//! else in the crate needs to know which database it is talking to.
//!
//! Bind variables are always positional `:1`, `:2`, ... in the SQL Kilat
//! builds: Oracle reads them natively, SQLite accepts `:1` as a named
//! parameter, and the driver of an engine with another spelling converts
//! the statement with [`rewrite_binds`] (see [`Dialect::bind_style`]). That
//! keeps the filter builder and [`crate::filter::BoundQuery`] previews
//! dialect-free.
//!
//! To add an engine: a [`DbKind`] variant, one file here implementing
//! [`Dialect`] (syntax, literals, type names), and a driver. Nothing else
//! in this crate matches on [`DbKind`].

mod bind;
pub(crate) mod literal;
mod mssql;
mod mysql;
mod oracle;
mod postgres;
mod sqlite;
#[cfg(test)]
mod tests;
mod types;

pub use bind::{rewrite as rewrite_binds, BindStyle, Rewritten};
pub use mssql::MssqlDialect;
pub use mysql::MysqlDialect;
pub use oracle::OracleDialect;
pub use postgres::PostgresDialect;
pub use sqlite::SqliteDialect;
pub use types::CanonType;

use crate::filter::TablePage;
use crate::model::{ColumnDef, Value};

/// Which database engine a connection profile points at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DbKind {
    #[default]
    Oracle,
    Sqlite,
    Postgres,
    Mysql,
    Mssql,
}

impl DbKind {
    pub const ALL: [Self; 5] = [
        Self::Oracle,
        Self::Sqlite,
        Self::Postgres,
        Self::Mysql,
        Self::Mssql,
    ];

    /// Name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Oracle => "Oracle",
            Self::Sqlite => "SQLite",
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Mssql => "SQL Server",
        }
    }

    /// Spelling stored in `profiles.json`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Oracle => "oracle",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Mssql => "mssql",
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
            Self::Postgres => &PostgresDialect,
            Self::Mysql => &MysqlDialect,
            Self::Mssql => &MssqlDialect,
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
    /// The server holds several databases that one session can read by
    /// naming them (`[db].[schema].[table]`). The explorer's dropdown then
    /// picks a database, objects are grouped by schema, and every data tab
    /// is bound to the database it was opened in. SQL Server only; MySQL
    /// calls its databases schemas.
    pub databases: bool,
    /// Materialized views, and "Refresh Materialized View".
    pub materialized_views: bool,
    /// Connecting with a username and password (and saving it in the
    /// keychain). SQLite opens a file.
    pub password: bool,
    /// Connecting to a server by host, port and database name (shown as
    /// separate fields). Oracle also takes a TNS alias, SQLite a file.
    pub network: bool,
    /// Rows have a physical id the data tab can select and edit by (Oracle
    /// `ROWID`, SQLite `rowid`, PostgreSQL `ctid`). Without one the grid is
    /// read-only until a key based identity exists.
    pub row_id: bool,
    /// "Connect as SYSDBA".
    pub sysdba: bool,
    /// DDL (CREATE, DROP, RENAME, TRUNCATE) is part of the open transaction:
    /// pending until Commit and undone by Rollback. Oracle and MySQL commit
    /// DDL at once.
    pub transactional_ddl: bool,
    /// The data tab can edit rows: by a physical row id, or by the primary
    /// key where the driver finds one (a table without either stays
    /// read-only).
    pub row_edit: bool,
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
    /// Recycle Bin: Flashback Table and PURGE.
    pub recycle_bin: bool,
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

    /// Like [`Dialect::table_ref`], qualified with `database` when the
    /// engine can reach other databases of the server from one session
    /// (`[db].[schema].[table]`). Engines without that ignore it.
    fn table_ref_in(&self, database: Option<&str>, owner: &str, table: &str) -> String {
        let _ = database;
        self.table_ref(owner, table)
    }

    /// Expression selecting the row id of table alias `t`, e.g. `t.ROWID`.
    fn row_id_expr(&self) -> &'static str;

    /// `WHERE` condition matching one row by the row id in bind `:n`.
    fn row_id_match(&self, bind: usize) -> String;

    /// Expression to `ORDER BY` for the row id; differs from
    /// [`Dialect::row_id_expr`] where the selected form is text that does
    /// not sort like the id itself (PostgreSQL `ctid::text`).
    fn row_id_sort_expr(&self) -> &'static str {
        self.row_id_expr()
    }

    /// Condition for rows after the row id `after`, for keyset paging in
    /// Data Transfer. `key` is [`Dialect::row_id_sort_expr`]. The value is
    /// checked: it ends up inside SQL text.
    fn keyset_after(&self, key: &str, after: &Value) -> Result<String, String>;

    /// Append the row window for `page` to a `SELECT`. Offset and limit
    /// are `usize`s written as literals, so they cannot carry SQL.
    fn push_page(&self, sql: &mut String, page: TablePage) {
        sql.push_str(&format!(" LIMIT {}", page.limit));
        if page.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", page.offset));
        }
    }

    /// Like [`Dialect::push_page`], for an engine that cannot page without
    /// an `ORDER BY` (SQL Server). `order_by` holds the key columns to sort
    /// by, unquoted; the default ignores them.
    fn push_page_ordered(&self, sql: &mut String, page: TablePage, order_by: &[String]) {
        let _ = order_by;
        self.push_page(sql, page);
    }

    /// How a driver of this engine writes bind variables.
    fn bind_style(&self) -> BindStyle {
        BindStyle::Colon
    }

    /// Port a server of this engine listens on by default.
    fn default_port(&self) -> Option<u16> {
        None
    }

    /// Label of the form field that names the database on the server.
    fn database_label(&self) -> &'static str {
        "Database"
    }

    /// Column types: the engine's type text as a neutral [`CanonType`] ...
    fn canonical_type(&self, native: &str) -> CanonType;

    /// ... and a neutral type as this engine's type text.
    fn native_type(&self, canonical: &CanonType) -> String;

    /// A column the database fills in by itself (identity, sequence), so a
    /// duplicated row leaves it empty.
    fn is_auto_generated(&self, column: &ColumnDef) -> bool {
        let _ = column;
        false
    }

    /// `DEFAULT` clause of a column definition for the expression `expr`.
    fn default_clause(&self, expr: &str) -> String {
        format!(" DEFAULT {expr}")
    }

    /// Whether a `FOREIGN KEY` is written inside `CREATE TABLE` (the
    /// parent must exist then) or added afterwards with `ALTER TABLE`.
    fn foreign_keys_inline(&self) -> bool {
        false
    }

    /// The type a column needs when it is part of a key or an index, if
    /// the usual one cannot be indexed (MySQL cannot key a `LONGTEXT`).
    fn key_type(&self, _native: &str) -> Option<String> {
        None
    }

    /// The parent table in a `REFERENCES` clause.
    fn foreign_key_parent(&self, schema: &str, parent: &str) -> String {
        self.table_ref(schema, parent)
    }

    /// `(index, table)` as written in `CREATE INDEX <index> ON <table>`.
    fn index_refs(&self, schema: &str, index: &str, table: &str) -> (String, String) {
        (self.table_ref(schema, index), self.table_ref(schema, table))
    }

    /// Starting types of the New Table dialog: the key column's and a
    /// text column's.
    fn new_table_types(&self) -> (&'static str, &'static str);

    /// `DROP <noun> ...` for an object of `noun` (upper-case, e.g.
    /// `INDEX`); `parent` is the table of a trigger or constraint. `None`
    /// when this engine cannot drop it from the name alone.
    fn drop_object(
        &self,
        noun: &str,
        owner: &str,
        name: &str,
        parent: Option<&str>,
    ) -> Option<String> {
        let _ = parent;
        Some(format!("DROP {noun} {}", self.table_ref(owner, name)))
    }

    /// `RENAME` of a table, `new` already valid.
    fn rename_table(&self, owner: &str, old: &str, new: &str) -> String {
        format!(
            "ALTER TABLE {} RENAME TO {}",
            self.table_ref(owner, old),
            self.quote_ident(new)
        )
    }

    /// A date (`YYYY-MM-DD`, already validated) as a literal, for the
    /// ranges of the recordset generator.
    fn date_literal(&self, iso: &str) -> String {
        format!("DATE '{iso}'")
    }

    /// Bytes as a literal. Errors say why, never show the value.
    fn bytes_literal(&self, bytes: &[u8]) -> Result<String, String>;

    /// Text as a literal for a column of `column_type` (the target's own
    /// type text): numbers and dates go in as such when the column wants
    /// them. Errors say why, never show the value.
    fn text_literal(&self, column_type: &str, text: &str) -> Result<String, String>;
}

/// Shared by the dialects whose columns of a numeric type take a bare
/// number: whether `base_type` (upper-case, no parameters) is one.
pub(crate) fn numeric_column(base: &str) -> bool {
    ["INT", "NUM", "REAL", "FLOA", "DOUB", "DEC", "SERIAL"]
        .iter()
        .any(|k| base.contains(k))
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn kind_keys_round_trip_and_default_to_oracle() {
        for kind in DbKind::ALL {
            assert_eq!(DbKind::from_key(kind.key()), kind);
            assert_eq!(kind.dialect().kind(), kind);
        }
        assert_eq!(DbKind::from_key(""), DbKind::Oracle);
        assert_eq!(DbKind::from_key("SQLite"), DbKind::Sqlite);
        assert_eq!(DbKind::from_key(" Postgres "), DbKind::Postgres);
        assert_eq!(DbKind::from_key("MYSQL"), DbKind::Mysql);
        assert_eq!(DbKind::from_key("mystery"), DbKind::Oracle);
    }

    #[test]
    fn shared_quoting_doubles_quotes() {
        assert_eq!(OracleDialect.table_ref("HR", "A\"B"), "\"HR\".\"A\"\"B\"");
        assert_eq!(SqliteDialect.table_ref("main", "t"), "\"main\".\"t\"");
        assert_eq!(
            PostgresDialect.table_ref("public", "a\"b"),
            "\"public\".\"a\"\"b\""
        );
    }

    #[test]
    fn mysql_quotes_with_backticks() {
        assert_eq!(MysqlDialect.table_ref("db", "a`b"), "`db`.`a``b`");
    }

    #[test]
    fn network_engines_have_a_default_port() {
        for kind in DbKind::ALL {
            let d = kind.dialect();
            assert_eq!(
                d.default_port().is_some(),
                d.capabilities().network,
                "{kind:?}"
            );
        }
    }
}
