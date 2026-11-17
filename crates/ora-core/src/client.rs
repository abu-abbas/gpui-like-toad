use std::fmt;

use crate::completion::CatalogObject;
use crate::filter::{self, TableFilter, TablePage};
use crate::model::IndexDef;
use crate::model::{ColumnDef, ConnectionProfile, ObjectKind, PlanNode, QueryResult, SchemaObject};

/// Error from a database operation. The message is user-facing: driver errors
/// are converted to their display text (including the ORA- number) so the UI
/// can show exactly what the database said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError(pub String);

impl DbError {
    pub fn new(message: impl Into<String>) -> Self {
        DbError(message.into())
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DbError {}

/// Information about an established session, shown in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInfo {
    /// Connected username, e.g. "HR".
    pub user: String,
    /// Database/service name, when the driver reports one.
    pub database: String,
    /// Database version string, when the driver reports one.
    pub version: String,
}

/// The single seam between the GUI and any Oracle access implementation.
///
/// Implementations must be `Send`: the GUI owns the client on a dedicated
/// background thread and never touches it from the UI thread. All methods are
/// blocking; the caller is responsible for running them off the UI thread.
///
/// Lifecycle: `connect` opens one session, queries reuse it, `disconnect`
/// closes it. Dropping a connected client must also close the session
/// (rollback of uncommitted work follows the driver's default).
pub trait DbClient: Send {
    /// Open a session. `password` is passed separately and must not be stored
    /// by the implementation beyond the connect call itself.
    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError>;

    /// Close the session, if any. Must be safe to call when disconnected.
    fn disconnect(&mut self);

    fn is_connected(&self) -> bool;

    /// Run a SELECT/WITH statement and fetch up to `max_rows` rows.
    /// Implementations strip a trailing `;`/`/` before executing (the thin
    /// driver rejects them) and set `QueryResult::truncated` when the cap
    /// cut the result short.
    fn query(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;

    /// Like [`DbClient::query`], with positional bind values (`:1` is
    /// `binds[0]`), all bound as text; Oracle converts them to the column's
    /// type when comparing. The default only supports an empty bind list.
    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        max_rows: usize,
    ) -> Result<QueryResult, DbError> {
        if binds.is_empty() {
            self.query(sql, max_rows)
        } else {
            Err(DbError::new(
                "bind variables are not supported by this backend",
            ))
        }
    }

    /// One page of the rows of `owner.table` that pass `filter`: the data
    /// tab's fetch. `QueryResult::truncated` is set when more rows follow
    /// the page. Built with [`filter::select_sql`], so filter
    /// values travel as bind variables.
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        let query = filter::select_sql(owner, table, filter, page);
        self.query_binds(&query.sql, &query.binds, page.limit)
    }

    /// Run a non-SELECT statement (DML/DDL/PL/SQL). Returns rows affected
    /// when the database reports a count, else 0. Does not commit.
    fn execute(&mut self, sql: &str) -> Result<u64, DbError>;

    /// Like [`DbClient::execute`], with positional text binds. The default
    /// only supports an empty bind list.
    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        if binds.is_empty() {
            self.execute(sql)
        } else {
            Err(DbError::new(
                "bind variables are not supported by this backend",
            ))
        }
    }

    /// Set one cell (`None` = NULL) of the row with `rowid`. Does not
    /// commit. Exactly one row must change: anything else is an error, so
    /// a stale ROWID can never touch the wrong row.
    fn update_cell(
        &mut self,
        owner: &str,
        table: &str,
        column: &str,
        rowid: &str,
        value: Option<&str>,
    ) -> Result<(), DbError> {
        let query = crate::edit::update_cell_sql(owner, table, column, rowid, value);
        match self.execute_binds(&query.sql, &query.binds)? {
            1 => Ok(()),
            0 => Err(DbError::new(
                "baris tidak ditemukan lagi (sudah dihapus atau ROWID berubah)",
            )),
            n => Err(DbError::new(format!(
                "UPDATE mengubah {n} baris, seharusnya 1; lakukan Rollback"
            ))),
        }
    }

    fn commit(&mut self) -> Result<(), DbError>;

    fn rollback(&mut self) -> Result<(), DbError>;

    /// Objects owned by `owner` (case-insensitive), grouped by the UI using
    /// [`crate::ObjectKind::group_label`].
    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError>;

    /// Schemas (users) the session can browse, sorted by name. Feeds the
    /// schema dropdown in the explorer.
    fn list_schemas(&mut self) -> Result<Vec<String>, DbError>;

    /// The object's DDL script (CREATE statement), as Toad's "View Script".
    fn object_ddl(&mut self, owner: &str, name: &str, kind: &ObjectKind)
        -> Result<String, DbError>;

    /// Insert rows into `owner.table`, one statement per row, values given
    /// as text in `columns` order. Does not commit. Returns rows inserted.
    /// Stops at the first failing row and reports its 1-based number; rows
    /// before it stay inserted (uncommitted) so the user can roll back.
    fn insert_rows(
        &mut self,
        owner: &str,
        table: &str,
        columns: &[String],
        rows: &[Vec<String>],
    ) -> Result<u64, DbError> {
        let mut inserted = 0u64;
        for (ix, row) in rows.iter().enumerate() {
            let sql = crate::import::insert_statement(owner, table, columns, row);
            self.execute(&sql)
                .map_err(|e| DbError::new(format!("baris data {}: {e}", ix + 1)))?;
            inserted += 1;
        }
        Ok(inserted)
    }

    /// Columns of one table or view.
    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError>;

    /// Everything the editor's autocomplete needs for one schema: its
    /// objects, with columns for tables and views. The default asks per
    /// table; backends with a data dictionary should override it with one
    /// bulk query, since a real schema can hold thousands of tables.
    fn catalog_objects(&mut self, owner: &str) -> Result<Vec<CatalogObject>, DbError> {
        let objects = self.list_objects(owner)?;
        let mut out = Vec::with_capacity(objects.len());
        for object in objects {
            let columns = match object.kind {
                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => self
                    .describe_table(&object.owner, &object.name)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            out.push(CatalogObject {
                name: object.name,
                kind: object.kind,
                columns,
            });
        }
        Ok(out)
    }

    /// Indexes on one table, sorted by name, for the data tab's Index
    /// view. A table without indexes (or a view) is an empty list.
    fn list_indexes(&mut self, _owner: &str, _table: &str) -> Result<Vec<IndexDef>, DbError> {
        Err(DbError::new("indexes are not supported by this backend"))
    }

    /// `EXPLAIN PLAN` output as formatted text lines (DBMS_XPLAN style), one
    /// string per line, without trailing newlines.
    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError>;

    /// `EXPLAIN PLAN` output as plan-tree nodes, from a hierarchical
    /// `PLAN_TABLE` query. Backends that cannot provide nodes keep the
    /// default, which reports unsupported; the UI then falls back to
    /// [`DbClient::explain_plan`] text.
    fn explain_plan_nodes(&mut self, _sql: &str) -> Result<Vec<PlanNode>, DbError> {
        Err(DbError::new("plan nodes are not supported by this backend"))
    }
}
