//! SQLite implementation of [`ora_core::DbClient`].
//!
//! A profile's `connect_string` is the database file path. SQLite is
//! compiled into the app (`rusqlite`'s `bundled` feature), so there is
//! nothing to install.
//!
//! SQLite commits every statement on its own unless a transaction is open.
//! To keep the app's "changes stay pending until Commit" model, the client
//! opens a transaction (`BEGIN`) before any statement whenever none is
//! open, and Commit/Rollback end it.
//!
//! Catalog queries (objects, columns, indexes, DDL) live in [`catalog`],
//! `EXPLAIN QUERY PLAN` in [`plan`]; this file is the session and plain
//! query/execute.

mod catalog;
mod plan;

use std::path::Path;
use std::time::Instant;

use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, IndexDef,
    ObjectKind, PlanNode, QueryResult, SchemaObject, TableFilter, TablePage, Value,
};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Statement};

/// Blocking SQLite client, owned by the GUI's background DB worker thread.
#[derive(Default)]
pub struct SqliteClient {
    conn: Option<Connection>,
}

impl SqliteClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn conn(&self) -> Result<&Connection, DbError> {
        self.conn
            .as_ref()
            .ok_or_else(|| DbError::new("not connected"))
    }

    /// The connection, with a transaction open so the next statement's
    /// changes wait for Commit.
    fn conn_in_tx(&self) -> Result<&Connection, DbError> {
        let conn = self.conn()?;
        if conn.is_autocommit() {
            conn.execute_batch("BEGIN").map_err(db_err)?;
        }
        Ok(conn)
    }
}

pub(crate) fn db_err(err: rusqlite::Error) -> DbError {
    DbError::new(format!("SQLite: {err}"))
}

/// Bind `binds[0]` to `:1`, `binds[1]` to `:2`, ... by name, so the order
/// the placeholders appear in the SQL does not matter.
fn bind_all(stmt: &mut Statement<'_>, binds: &[String]) -> Result<(), DbError> {
    for (ix, value) in binds.iter().enumerate() {
        let name = format!(":{}", ix + 1);
        let pos = stmt
            .parameter_index(&name)
            .map_err(db_err)?
            .ok_or_else(|| DbError::new(format!("SQLite: no bind variable {name} in statement")))?;
        stmt.raw_bind_parameter(pos, value).map_err(db_err)?;
    }
    Ok(())
}

fn cell_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::Integer(i128::from(n)),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

pub(crate) fn fetch_rows(
    conn: &Connection,
    sql: &str,
    binds: &[String],
    max_rows: usize,
) -> Result<QueryResult, DbError> {
    let started = Instant::now();
    let mut stmt = conn.prepare(clean_statement(sql)).map_err(db_err)?;
    bind_all(&mut stmt, binds)?;
    let columns: Vec<ColumnInfo> = stmt
        .columns()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            data_type: c.decl_type().unwrap_or("").to_ascii_uppercase(),
        })
        .collect();
    let width = columns.len();
    let fetching = Instant::now();
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut cursor = stmt.raw_query();
    while let Some(row) = cursor.next().map_err(db_err)? {
        if rows.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut values = Vec::with_capacity(width);
        for ix in 0..width {
            values.push(cell_value(row.get_ref(ix).map_err(db_err)?));
        }
        rows.push(values);
    }
    Ok(QueryResult {
        columns,
        rows,
        truncated,
        fetch_time: Some(fetching.elapsed()),
        elapsed: started.elapsed(),
    })
}

impl DbClient for SqliteClient {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }

    fn connect(&mut self, profile: &ConnectionProfile, _password: &str) -> Result<DbInfo, DbError> {
        let path = profile.connect_string.trim();
        if path.is_empty() {
            return Err(DbError::new("Select a SQLite database file first."));
        }
        // No CREATE flag: a typo in the path must not leave an empty
        // database file behind.
        if !Path::new(path).is_file() {
            return Err(DbError::new(format!(
                "SQLite database file not found: {path}"
            )));
        }
        let flags = if profile.access.read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(db_err)?;
        // Reading the schema fails fast on a file that is not a database.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(db_err)?;
        let version = rusqlite::version();
        let database = Path::new(path)
            .file_name()
            .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
        self.conn = Some(conn);
        Ok(DbInfo {
            user: "main".to_string(),
            database,
            version: format!("SQLite {version}"),
        })
    }

    fn disconnect(&mut self) {
        // Dropping the connection rolls back an open transaction.
        self.conn = None;
    }

    fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    fn ping(&self) -> Result<(), DbError> {
        self.conn()?
            .query_row("SELECT 1", [], |_| Ok(()))
            .map_err(db_err)
    }

    fn query(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        self.query_binds(sql, &[], max_rows)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        max_rows: usize,
    ) -> Result<QueryResult, DbError> {
        // Reads do not open a transaction: that would hold a read lock on
        // the file until Commit and block other programs from writing.
        fetch_rows(self.conn()?, sql, binds, max_rows)
    }

    /// Tables declared `WITHOUT ROWID` have no row id to edit by, so their
    /// pages are fetched without one (the grid then stays read-only).
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        let page = if page.rowid && catalog::without_rowid(self.conn()?, owner, table)? {
            page.with_rowid(false)
        } else {
            page
        };
        let query = ora_core::filter::select_sql(self.dialect(), owner, table, filter, page);
        self.query_binds(&query.sql, &query.binds, page.limit)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        self.execute_binds(sql, &[])
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        let conn = self.conn_in_tx()?;
        let mut stmt = conn.prepare(clean_statement(sql)).map_err(db_err)?;
        bind_all(&mut stmt, binds)?;
        let changed = stmt.raw_execute().map_err(db_err)?;
        Ok(changed as u64)
    }

    fn commit(&mut self) -> Result<(), DbError> {
        let conn = self.conn()?;
        if !conn.is_autocommit() {
            conn.execute_batch("COMMIT").map_err(db_err)?;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        let conn = self.conn()?;
        if !conn.is_autocommit() {
            conn.execute_batch("ROLLBACK").map_err(db_err)?;
        }
        Ok(())
    }

    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        catalog::list_objects(self.conn()?, owner)
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        catalog::list_schemas(self.conn()?)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        _kind: &ObjectKind,
    ) -> Result<String, DbError> {
        catalog::object_ddl(self.conn()?, owner, name)
    }

    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError> {
        catalog::describe_table(self.conn()?, owner, name)
    }

    fn list_indexes(&mut self, owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        catalog::list_indexes(self.conn()?, owner, table)
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        Ok(plan::as_text(&plan::nodes(self.conn()?, sql)?))
    }

    fn explain_plan_nodes(&mut self, sql: &str) -> Result<Vec<PlanNode>, DbError> {
        plan::nodes(self.conn()?, sql)
    }
}

#[cfg(test)]
mod tests;
