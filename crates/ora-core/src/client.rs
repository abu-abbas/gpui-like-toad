use std::fmt;

use crate::model::{ColumnDef, ConnectionProfile, PlanNode, QueryResult, SchemaObject};

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

    /// Run a non-SELECT statement (DML/DDL/PL/SQL). Returns rows affected
    /// when the database reports a count, else 0. Does not commit.
    fn execute(&mut self, sql: &str) -> Result<u64, DbError>;

    fn commit(&mut self) -> Result<(), DbError>;

    fn rollback(&mut self) -> Result<(), DbError>;

    /// Objects owned by `owner` (case-insensitive), grouped by the UI using
    /// [`crate::ObjectKind::group_label`].
    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError>;

    /// Columns of one table or view.
    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError>;

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
