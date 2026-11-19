//! Background database worker.
//!
//! The Oracle driver is blocking, so exactly one worker thread owns the
//! `DbClient` for the whole session. The UI thread never touches the client:
//! it sends [`Job`]s over a channel and awaits [`Response`]s on GPUI's
//! background executor. One job runs at a time; the UI enforces that with a
//! busy flag so results never interleave.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ora_core::completion::CatalogObject;
use ora_core::credentials::{CredentialStore as _, ExposeSecret as _, SecretString};
use ora_core::demo::DemoClient;
use ora_core::export::{to_csv, to_xlsx};
use ora_core::import::{map_columns, parse_csv};
use ora_core::sql::statement_kind;
use ora_core::{
    ColumnDef, ConnectionAccess, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, PlanNode, QueryResult, SchemaObject, TableFilter, TablePage, Value,
};

/// Bytes all result sets of one script may keep together. Each statement
/// gets what is left (at most the per-result limit), so a script of many
/// SELECTs cannot keep many full-size results.
pub const SCRIPT_MAX_RESULT_BYTES: usize = 2 * ora_core::fetch::MAX_RESULT_BYTES;

/// File format for "Export Data" on a whole table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Xlsx,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Xlsx => "xlsx",
        }
    }
}
use ora_driver::ThinClient;
use sqlite_driver::SqliteClient;

/// Where a connect gets its password. `Saved` is read from the OS keychain
/// here on the worker thread, so a keychain prompt never blocks the UI and
/// the password never passes through UI state.
pub enum Password {
    Typed(SecretString),
    Saved { profile_id: String },
}

/// Message for a profile marked "Save password" whose keychain entry is
/// gone (removed by hand, or saved on another machine).
pub const NO_SAVED_PASSWORD: &str =
    "The saved password is missing from the keychain. Enter the password and click Connect.";

impl Password {
    fn resolve(self) -> Result<SecretString, String> {
        match self {
            Password::Typed(pw) => Ok(pw),
            Password::Saved { profile_id } => match crate::keychain::store().get(&profile_id) {
                Ok(Some(pw)) => Ok(pw),
                Ok(None) => Err(NO_SAVED_PASSWORD.to_string()),
                Err(err) => Err(err.to_string()),
            },
        }
    }
}

/// One cell edit of a data tab: the row (by ROWID), the column and the new
/// value (`None` = NULL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellUpdate {
    pub rowid: String,
    pub column: String,
    pub value: Option<String>,
}

pub enum Job {
    Connect {
        profile: ConnectionProfile,
        password: Password,
        demo: bool,
    },
    /// Open a throwaway session with the given profile and close it again,
    /// without touching the worker's current session. Powers the
    /// connection manager's Test button.
    TestConnect {
        profile: ConnectionProfile,
        password: Password,
    },
    /// Run the editor text: SELECT goes to `query()`, anything else to
    /// `execute()`. Decided here so the UI has one code path.
    Run {
        sql: String,
    },
    /// Data tab fetch: rows of one table/view that pass the filter bar's
    /// conditions, values sent as bind variables.
    SelectScript {
        statements: Vec<String>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    QueryTable {
        owner: String,
        name: String,
        filter: TableFilter,
        page: TablePage,
    },
    /// Data tab cell edits: one `UPDATE` per cell, in order, stopping at
    /// the first failure. Not committed.
    UpdateCells {
        owner: String,
        name: String,
        edits: Vec<CellUpdate>,
        deletes: Vec<String>,
        inserts: Vec<Vec<(String, String)>>,
    },
    /// Explorer "Refresh Materialized View": `DBMS_MVIEW.REFRESH`.
    RefreshMaterializedView {
        owner: String,
        name: String,
    },
    Commit,
    Rollback,
    ListSchemas,
    ListObjects {
        owner: String,
    },
    /// DDL script for the explorer's "View Script".
    Ddl {
        owner: String,
        name: String,
        kind: ObjectKind,
    },
    /// Explorer "Export Data": fetch the whole table (capped at
    /// [`ora_core::EXPORT_MAX_ROWS`]) and write it to `path`.
    ExportTable {
        owner: String,
        name: String,
        path: PathBuf,
        format: ExportFormat,
    },
    /// Explorer "Import Data": insert every row of a CSV file whose header
    /// names match the table's columns. Not committed.
    ImportCsv {
        owner: String,
        name: String,
        path: PathBuf,
    },
    Describe {
        owner: String,
        name: String,
    },
    /// "Load full value": one cell of a data tab, by ROWID, fetched with
    /// the full-value limit instead of the grid's per-cell limit.
    LoadCell {
        owner: String,
        name: String,
        column: String,
        rowid: String,
        /// Most bytes the value may take: what the open results can still
        /// hold. A bigger value is refused, not loaded.
        budget: usize,
    },
    /// Indexes of one table, for the data tab's Index view.
    Indexes {
        owner: String,
        name: String,
    },
    Explain {
        sql: String,
    },
    /// Objects and columns of one schema, for the editor's autocomplete.
    Catalog {
        owner: String,
    },
    Ping,
    Disconnect,
}

impl Job {
    pub(crate) fn protected_operation(&self) -> Option<&'static str> {
        use ora_core::sql::is_read_query;
        match self {
            Self::Run { sql } if !is_read_query(sql) => Some("Execute SQL"),
            Self::SelectScript { statements, .. }
                if !statements.iter().all(|sql| is_read_query(sql)) =>
            {
                Some("Run script")
            }
            Self::UpdateCells { .. } => Some("Apply table changes"),
            Self::ImportCsv { .. } => Some("Import CSV rows"),
            Self::Explain { .. } => Some("Write to the Oracle plan table"),
            Self::RefreshMaterializedView { .. } => Some("Refresh materialized view"),
            Self::Commit => Some("Commit changes"),
            Self::Rollback => Some("Roll back changes"),
            _ => None,
        }
    }
}

pub struct StatementResult {
    pub sql: String,
    pub started: String,
    /// Shared: the statement list, the tab and the grid hold the same
    /// result instead of copies.
    pub outcome: Result<std::sync::Arc<QueryResult>, String>,
}

pub enum Response {
    ScriptDone(Vec<StatementResult>),
    Connected(DbInfo),
    Tested(Result<DbInfo, String>),
    QueryDone(QueryResult),
    Executed(u64),
    Committed,
    RolledBack,
    Schemas(Vec<String>),
    Objects(Vec<SchemaObject>),
    Ddl(String),
    Exported {
        path: PathBuf,
        rows: usize,
        truncated: bool,
        /// Cells longer than the export's per-cell limit (written cut).
        clipped: usize,
    },
    /// The full value of one cell ("Load full value").
    CellLoaded {
        rowid: String,
        column: String,
        value: Value,
    },
    Imported(u64),
    /// How many of the cell edits went through, and why the next one
    /// failed, if one did.
    CellsUpdated {
        done: usize,
        error: Option<String>,
    },
    Described(Vec<ColumnDef>),
    Indexes(Vec<IndexDef>),
    Plan(Vec<String>),
    /// Structured plan nodes for the tree panel, plus the DBMS_XPLAN text
    /// lines for the Explain Plan sub-tab (empty when the text path failed).
    PlanTree(Vec<PlanNode>, Vec<String>),
    Catalog(Vec<CatalogObject>),
    Alive,
    ConnectionLost(String),
    Disconnected,
    Failed(String),
}

/// Cloneable handle to the worker thread.
pub struct DbHandle {
    tx: Sender<(Job, bool, Sender<Response>)>,
}

impl Clone for DbHandle {
    fn clone(&self) -> Self {
        DbHandle {
            tx: self.tx.clone(),
        }
    }
}

impl DbHandle {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<(Job, bool, Sender<Response>)>();
        std::thread::spawn(move || {
            let mut client: Option<Box<dyn DbClient>> = None;
            let mut access = ConnectionAccess::default();
            let mut last_activity = Instant::now();
            let mut lost = None;
            while let Ok((job, confirmed, reply)) = rx.recv() {
                // A panic in one job must not kill the session: report it
                // and keep the worker (and its connection) alive.
                let response = catch_unwind(AssertUnwindSafe(|| {
                    handle_live_job(
                        job,
                        confirmed,
                        &mut client,
                        &mut access,
                        &mut last_activity,
                        &mut lost,
                    )
                }))
                .unwrap_or_else(|_| Response::Failed("internal error di DB worker".to_string()));
                let _ = reply.send(response);
            }
        });
        DbHandle { tx }
    }

    /// Send a job; the response arrives on the returned receiver. If the
    /// worker thread is gone, the receiver yields a `Failed` immediately
    /// instead of blocking forever.
    pub fn request(&self, job: Job) -> Receiver<Response> {
        self.dispatch(job, false)
    }

    pub(crate) fn request_confirmed(&self, job: Job) -> Receiver<Response> {
        self.dispatch(job, true)
    }

    fn dispatch(&self, job: Job, confirmed: bool) -> Receiver<Response> {
        let (tx, rx) = mpsc::channel();
        if self.tx.send((job, confirmed, tx.clone())).is_err() {
            let _ = tx.send(Response::Failed(
                "The database worker is not running".to_string(),
            ));
        }
        rx
    }
}

const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(45);

fn handle_live_job(
    job: Job,
    confirmed: bool,
    client: &mut Option<Box<dyn DbClient>>,
    access: &mut ConnectionAccess,
    last_activity: &mut Instant,
    lost: &mut Option<String>,
) -> Response {
    let lifecycle = matches!(
        job,
        Job::Connect { .. } | Job::Disconnect | Job::TestConnect { .. }
    );
    if !lifecycle {
        if let Some(reason) = lost {
            return Response::ConnectionLost(reason.clone());
        }
        if last_activity.elapsed() >= IDLE_CHECK_INTERVAL {
            if let Some(active) = client.as_ref() {
                if let Err(err) = active.ping() {
                    return mark_connection_lost(client, lost, err.to_string());
                }
                *last_activity = Instant::now();
            }
        }
    }
    if matches!(job, Job::Ping) {
        return Response::Alive;
    }
    let response = handle_guarded_job(job, confirmed, client, access);
    if matches!(response, Response::Connected(_) | Response::Disconnected) {
        *lost = None;
    }
    if !lifecycle {
        let operation_error = match &response {
            Response::Failed(error)
            | Response::CellsUpdated {
                error: Some(error), ..
            } => Some(error),
            Response::ScriptDone(results) => results.last().and_then(|r| r.outcome.as_ref().err()),
            _ => None,
        };
        if let (Some(active), Some(error)) = (client.as_ref(), operation_error) {
            if let Err(ping_error) = active.ping() {
                return mark_connection_lost(client, lost, format!("{error} (ping: {ping_error})"));
            }
        }
    }
    if !matches!(response, Response::Tested(_)) {
        *last_activity = Instant::now();
    }
    response
}

fn mark_connection_lost(
    client: &mut Option<Box<dyn DbClient>>,
    lost: &mut Option<String>,
    reason: String,
) -> Response {
    // Do not replay a pending write or commit after reconnecting. A response
    // could have been lost after Oracle executed the operation.
    *lost = Some(reason.clone());
    *client = None;
    Response::ConnectionLost(reason)
}

fn handle_guarded_job(
    job: Job,
    confirmed: bool,
    client: &mut Option<Box<dyn DbClient>>,
    access: &mut ConnectionAccess,
) -> Response {
    if let Some(operation) = job.protected_operation() {
        if access.read_only {
            return Response::Failed(format!("{operation}: this connection is Read Only."));
        }
        if access.protect_production && !confirmed {
            return Response::Failed(format!("{operation}: production confirmation is required."));
        }
    }
    let requested = match &job {
        Job::Connect { profile, .. } => Some(profile.access),
        _ => None,
    };
    let result = handle_job(job, client);
    match result {
        Response::Connected(_) => *access = requested.unwrap_or_default(),
        Response::Disconnected => *access = ConnectionAccess::default(),
        _ => {}
    }
    result
}

/// A fresh, unconnected client for `kind`. Each database engine gets its
/// own driver crate; this is the only place that picks one.
fn new_client(kind: DbKind, demo: bool) -> Box<dyn DbClient> {
    if demo {
        return Box::new(DemoClient::new());
    }
    match kind {
        DbKind::Oracle => Box::new(ThinClient::new()),
        DbKind::Sqlite => Box::new(SqliteClient::new()),
    }
}

fn handle_job(job: Job, client: &mut Option<Box<dyn DbClient>>) -> Response {
    match job {
        Job::Ping => Response::Alive,
        Job::Connect {
            profile,
            password,
            demo,
        } => {
            let password = match password.resolve() {
                Ok(pw) => pw,
                Err(message) => return Response::Failed(message),
            };
            let mut new_client = new_client(profile.kind, demo);
            // `password` is zeroed when it drops at the end of this arm.
            match new_client.connect(&profile, password.expose_secret()) {
                Ok(info) => {
                    if let Some(mut old) = client.replace(new_client) {
                        old.disconnect();
                    }
                    Response::Connected(info)
                }
                Err(err) => Response::Failed(err.to_string()),
            }
        }
        Job::Disconnect => {
            if let Some(mut old) = client.take() {
                old.disconnect();
            }
            Response::Disconnected
        }
        Job::TestConnect { profile, password } => {
            // A separate throwaway client: testing must never disturb the
            // live session (or log it out).
            let password = match password.resolve() {
                Ok(pw) => pw,
                Err(message) => return Response::Tested(Err(message)),
            };
            let mut probe = new_client(profile.kind, false);
            let outcome = probe
                .connect(&profile, password.expose_secret())
                .map_err(|e| e.to_string());
            probe.disconnect();
            Response::Tested(outcome)
        }
        Job::SelectScript {
            statements,
            cancelled,
        } => {
            let Some(client) = client.as_mut() else {
                return Response::Failed("not connected".into());
            };
            let mut results = Vec::new();
            let mut budget = SCRIPT_MAX_RESULT_BYTES;
            for sql in statements {
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let started = chrono::Local::now()
                    .format("%Y-%m-%d %H:%M:%S %:z")
                    .to_string();
                let limits = script_limits(budget);
                let outcome = client
                    .query(&sql, limits)
                    .map(|result| {
                        budget = budget.saturating_sub(result.heap_bytes());
                        std::sync::Arc::new(result)
                    })
                    .map_err(|e| e.to_string());
                let failed = outcome.is_err();
                results.push(StatementResult {
                    sql,
                    started,
                    outcome,
                });
                if failed {
                    break;
                }
            }
            Response::ScriptDone(results)
        }
        Job::Run { sql } => {
            let Some(client) = client.as_mut() else {
                return Response::Failed("not connected".to_string());
            };
            match statement_kind(&sql) {
                ora_core::sql::StatementKind::Select => {
                    match client.query(&sql, FetchLimits::grid()) {
                        Ok(result) => Response::QueryDone(result),
                        Err(err) => Response::Failed(err.to_string()),
                    }
                }
                ora_core::sql::StatementKind::Empty => {
                    Response::Failed("empty statement".to_string())
                }
                _ => match client.execute(&sql) {
                    Ok(affected) => Response::Executed(affected),
                    Err(err) => Response::Failed(err.to_string()),
                },
            }
        }
        Job::QueryTable {
            owner,
            name,
            filter,
            page,
        } => match client.as_mut() {
            Some(client) => match client.query_table(&owner, &name, &filter, page) {
                Ok(result) => Response::QueryDone(result),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::UpdateCells {
            owner,
            name,
            edits,
            deletes,
            inserts,
        } => match client.as_mut() {
            Some(client) => {
                let mut done = 0;
                let mut error = None;
                for edit in &edits {
                    let result = client.update_cell(
                        &owner,
                        &name,
                        &edit.column,
                        &edit.rowid,
                        edit.value.as_deref(),
                    );
                    match result {
                        Ok(()) => done += 1,
                        Err(err) => {
                            error = Some(format!("{}: {err}", edit.column));
                            break;
                        }
                    }
                }
                for rowid in deletes {
                    if error.is_some() {
                        break;
                    }
                    match client.delete_grid_row(&owner, &name, &rowid) {
                        Ok(()) => done += 1,
                        Err(err) => error = Some(err.to_string()),
                    }
                }
                for insert in inserts {
                    if error.is_some() {
                        break;
                    }
                    match client.insert_grid_row(&owner, &name, &insert) {
                        Ok(()) => done += 1,
                        Err(err) => error = Some(err.to_string()),
                    }
                }
                Response::CellsUpdated { done, error }
            }
            None => Response::Failed("not connected".to_string()),
        },
        Job::Commit => match client.as_mut() {
            Some(client) => match client.commit() {
                Ok(()) => Response::Committed,
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Rollback => match client.as_mut() {
            Some(client) => match client.rollback() {
                Ok(()) => Response::RolledBack,
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::ListSchemas => match client.as_mut() {
            Some(client) => match client.list_schemas() {
                Ok(schemas) => Response::Schemas(schemas),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Ddl { owner, name, kind } => match client.as_mut() {
            Some(client) => match client.object_ddl(&owner, &name, &kind) {
                Ok(ddl) => Response::Ddl(ddl),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::ExportTable {
            owner,
            name,
            path,
            format,
        } => match client.as_mut() {
            Some(client) => match export_table(client.as_mut(), &owner, &name, &path, format) {
                Ok((rows, truncated, clipped)) => Response::Exported {
                    path,
                    rows,
                    truncated,
                    clipped,
                },
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::RefreshMaterializedView { owner, name } => match client.as_mut() {
            Some(client) => match client.execute_binds(
                ora_core::import::REFRESH_MVIEW_SQL,
                &[ora_core::import::mview_list_name(&owner, &name)],
            ) {
                Ok(_) => Response::Executed(0),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::ImportCsv { owner, name, path } => match client.as_mut() {
            Some(client) => match import_csv(client.as_mut(), &owner, &name, &path) {
                Ok(rows) => Response::Imported(rows),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::ListObjects { owner } => match client.as_mut() {
            Some(client) => match client.list_objects(&owner) {
                Ok(objects) => Response::Objects(objects),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Catalog { owner } => match client.as_mut() {
            Some(client) => match client.catalog_objects(&owner) {
                Ok(objects) => Response::Catalog(objects),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Describe { owner, name } => match client.as_mut() {
            Some(client) => match client.describe_table(&owner, &name) {
                Ok(defs) => Response::Described(defs),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::LoadCell {
            owner,
            name,
            column,
            rowid,
            budget,
        } => match client.as_mut() {
            Some(client) => match client.fetch_cell(&owner, &name, &column, &rowid, budget) {
                Ok(value) => Response::CellLoaded {
                    rowid,
                    column,
                    value,
                },
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Indexes { owner, name } => match client.as_mut() {
            Some(client) => match client.list_indexes(&owner, &name) {
                Ok(indexes) => Response::Indexes(indexes),
                Err(err) => Response::Failed(err.to_string()),
            },
            None => Response::Failed("not connected".to_string()),
        },
        Job::Explain { sql } => match client.as_mut() {
            Some(client) => {
                // Prefer structured plan nodes (tree view) with the
                // DBMS_XPLAN text alongside; fall back to text alone when
                // the backend cannot provide nodes. Both calls run their
                // own EXPLAIN PLAN, which is cheap and overwrites the same
                // PLAN_TABLE rows, so the two views always agree.
                let nodes = client.explain_plan_nodes(&sql);
                let lines = client.explain_plan(&sql);
                match (nodes, lines) {
                    (Ok(nodes), lines) if !nodes.is_empty() => {
                        Response::PlanTree(nodes, lines.unwrap_or_default())
                    }
                    (_, Ok(lines)) => Response::Plan(lines),
                    (_, Err(err)) => Response::Failed(err.to_string()),
                }
            }
            None => Response::Failed("not connected".to_string()),
        },
    }
}

fn export_table(
    client: &mut dyn DbClient,
    owner: &str,
    name: &str,
    path: &std::path::Path,
    format: ExportFormat,
) -> Result<(usize, bool, usize), DbError> {
    let sql = format!("SELECT * FROM {}", client.dialect().table_ref(owner, name));
    let result = client.query(&sql, FetchLimits::export())?;
    let bytes = match format {
        ExportFormat::Csv => to_csv(&result).into_bytes(),
        ExportFormat::Xlsx => to_xlsx(&result).map_err(DbError::new)?,
    };
    std::fs::write(path, bytes)
        .map_err(|e| DbError::new(format!("Failed to write {}: {e}", path.display())))?;
    Ok((result.row_count(), result.truncated, result.clipped_cells()))
}

/// Grid limits for the next statement of a script, given the bytes the
/// script's earlier results left over.
fn script_limits(budget_left: usize) -> FetchLimits {
    let mut limits = FetchLimits::grid();
    limits.max_result_bytes = limits.max_result_bytes.min(budget_left);
    limits
}

fn import_csv(
    client: &mut dyn DbClient,
    owner: &str,
    name: &str,
    path: &std::path::Path,
) -> Result<u64, DbError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| DbError::new(format!("Failed to read {}: {e}", path.display())))?;
    let csv = parse_csv(&text).map_err(DbError::new)?;
    let table_columns = client.describe_table(owner, name)?;
    let columns = map_columns(&csv.headers, &table_columns).map_err(DbError::new)?;
    client.insert_rows(owner, name, &columns, &csv.rows)
}

#[cfg(test)]
mod tests {
    #[test]
    fn refreshing_a_materialized_view_is_a_guarded_write() {
        let job = Job::RefreshMaterializedView {
            owner: "HR".into(),
            name: "MV_X".into(),
        };
        assert_eq!(job.protected_operation(), Some("Refresh materialized view"));
        let mut client: Option<Box<dyn DbClient>> = None;
        let mut access = ConnectionAccess {
            read_only: true,
            ..ConnectionAccess::default()
        };
        assert!(matches!(
            handle_guarded_job(job, false, &mut client, &mut access),
            Response::Failed(message) if message.contains("Read Only")
        ));
    }

    use super::*;

    fn demo() -> DemoClient {
        let mut client = DemoClient::new();
        client
            .connect(
                &ConnectionProfile {
                    name: "demo".into(),
                    kind: DbKind::Oracle,
                    username: "hr".into(),
                    connect_string: "demo".into(),
                    access: Default::default(),
                },
                "",
            )
            .unwrap();
        client
    }

    #[test]
    fn protected_jobs_require_confirmation_and_read_only_takes_precedence() {
        let mut client: Option<Box<dyn DbClient>> = Some(Box::new(demo()));
        let mut access = ConnectionAccess {
            protect_production: true,
            ..ConnectionAccess::default()
        };
        let write = || Job::Run {
            sql: "DELETE FROM departments".into(),
        };
        assert!(matches!(
            handle_guarded_job(write(), false, &mut client, &mut access),
            Response::Failed(_)
        ));
        assert!(matches!(
            handle_guarded_job(write(), true, &mut client, &mut access),
            Response::Executed(1)
        ));
        access.read_only = true;
        assert!(matches!(
            handle_guarded_job(write(), true, &mut client, &mut access),
            Response::Failed(_)
        ));
        let read = Job::Run {
            sql: "SELECT 1 FROM dual".into(),
        };
        assert!(matches!(
            handle_guarded_job(read, false, &mut client, &mut access),
            Response::QueryDone(_)
        ));
    }

    #[test]
    fn failed_idle_probe_prevents_later_queries_on_that_worker() {
        let mut client: Option<Box<dyn DbClient>> = Some(Box::new(DemoClient::new()));
        let mut access = ConnectionAccess::default();
        let mut activity = Instant::now() - IDLE_CHECK_INTERVAL;
        let mut lost = None;
        let ping = handle_live_job(
            Job::Ping,
            false,
            &mut client,
            &mut access,
            &mut activity,
            &mut lost,
        );
        assert!(matches!(ping, Response::ConnectionLost(_)));
        assert!(client.is_none());
        let write = Job::Run {
            sql: "DELETE FROM departments".into(),
        };
        let response = handle_live_job(
            write,
            false,
            &mut client,
            &mut access,
            &mut activity,
            &mut lost,
        );
        assert!(matches!(response, Response::ConnectionLost(_)));
    }

    #[test]
    fn select_script_retains_results_before_an_error() {
        let mut client: Option<Box<dyn DbClient>> = Some(Box::new(demo()));
        let script = "select 1 from dual; select * from missing; select 1 from dual;";
        let job = Job::SelectScript {
            statements: ora_core::sql::select_script(script).unwrap(),
            cancelled: Default::default(),
        };
        let Response::ScriptDone(results) = handle_job(job, &mut client) else {
            panic!("expected script results")
        };
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].outcome.as_ref().unwrap().row_count(), 1);
        assert!(results[1].outcome.is_err());
    }

    #[test]
    fn export_then_import_round_trips_through_a_file() {
        // Test-only scratch dir, removed at the end; no security decision
        // depends on it.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("gpui-like-toad-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("departments.csv");
        let mut client = demo();
        let (rows, truncated, _clipped) =
            export_table(&mut client, "HR", "DEPARTMENTS", &path, ExportFormat::Csv).unwrap();
        assert_eq!(rows, 4);
        assert!(!truncated);
        let inserted = import_csv(&mut client, "HR", "DEPARTMENTS", &path).unwrap();
        assert_eq!(inserted, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_rejects_unknown_columns() {
        // Test-only scratch dir, removed at the end; no security decision
        // depends on it.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("gpui-like-toad-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.csv");
        std::fs::write(&path, "DEPARTMENT_ID,NOPE\n1,2\n").unwrap();
        let err = import_csv(&mut demo(), "HR", "DEPARTMENTS", &path).unwrap_err();
        assert!(err.to_string().contains("NOPE"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
