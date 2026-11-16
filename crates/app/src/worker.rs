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

use ora_core::demo::DemoClient;
use ora_core::export::{to_csv, to_xlsx};
use ora_core::import::{map_columns, parse_csv, quote_ident};
use ora_core::sql::statement_kind;
use ora_core::{
    ColumnDef, ConnectionProfile, DbClient, DbError, DbInfo, ObjectKind, PlanNode, QueryResult,
    SchemaObject, DEFAULT_MAX_ROWS, EXPORT_MAX_ROWS,
};

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

pub enum Job {
    Connect {
        profile: ConnectionProfile,
        password: String,
        demo: bool,
    },
    /// Open a throwaway session with the given profile and close it again,
    /// without touching the worker's current session. Powers the
    /// connection manager's Test button.
    TestConnect {
        profile: ConnectionProfile,
        password: String,
    },
    /// Run the editor text: SELECT goes to `query()`, anything else to
    /// `execute()`. Decided here so the UI has one code path.
    Run {
        sql: String,
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
    /// [`EXPORT_MAX_ROWS`]) and write it to `path`.
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
    Explain {
        sql: String,
    },
    Disconnect,
}

pub enum Response {
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
    },
    Imported(u64),
    Described(Vec<ColumnDef>),
    Plan(Vec<String>),
    /// Structured plan nodes for the tree panel, plus the DBMS_XPLAN text
    /// lines for the Explain Plan sub-tab (empty when the text path failed).
    PlanTree(Vec<PlanNode>, Vec<String>),
    Disconnected,
    Failed(String),
}

/// Cloneable handle to the worker thread.
pub struct DbHandle {
    tx: Sender<(Job, Sender<Response>)>,
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
        let (tx, rx) = mpsc::channel::<(Job, Sender<Response>)>();
        std::thread::spawn(move || {
            let mut client: Option<Box<dyn DbClient>> = None;
            while let Ok((job, reply)) = rx.recv() {
                // A panic in one job must not kill the session: report it
                // and keep the worker (and its connection) alive.
                let response = catch_unwind(AssertUnwindSafe(|| handle_job(job, &mut client)))
                    .unwrap_or_else(|_| {
                        Response::Failed("internal error di DB worker".to_string())
                    });
                let _ = reply.send(response);
            }
        });
        DbHandle { tx }
    }

    /// Send a job; the response arrives on the returned receiver. If the
    /// worker thread is gone, the receiver yields a `Failed` immediately
    /// instead of blocking forever.
    pub fn request(&self, job: Job) -> Receiver<Response> {
        let (tx, rx) = mpsc::channel();
        if self.tx.send((job, tx.clone())).is_err() {
            let _ = tx.send(Response::Failed("DB worker tidak berjalan".to_string()));
        }
        rx
    }
}

fn handle_job(job: Job, client: &mut Option<Box<dyn DbClient>>) -> Response {
    match job {
        Job::Connect {
            profile,
            password,
            demo,
        } => {
            let mut new_client: Box<dyn DbClient> = if demo {
                Box::new(DemoClient::new())
            } else {
                Box::new(ThinClient::new())
            };
            match new_client.connect(&profile, &password) {
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
            let mut probe = ThinClient::new();
            let outcome = probe
                .connect(&profile, &password)
                .map_err(|e| e.to_string());
            probe.disconnect();
            Response::Tested(outcome)
        }
        Job::Run { sql } => {
            let Some(client) = client.as_mut() else {
                return Response::Failed("not connected".to_string());
            };
            match statement_kind(&sql) {
                ora_core::sql::StatementKind::Select => {
                    match client.query(&sql, DEFAULT_MAX_ROWS) {
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
                Ok((rows, truncated)) => Response::Exported {
                    path,
                    rows,
                    truncated,
                },
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
        Job::Describe { owner, name } => match client.as_mut() {
            Some(client) => match client.describe_table(&owner, &name) {
                Ok(defs) => Response::Described(defs),
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
) -> Result<(usize, bool), DbError> {
    let sql = format!("SELECT * FROM {}.{}", quote_ident(owner), quote_ident(name));
    let result = client.query(&sql, EXPORT_MAX_ROWS)?;
    let bytes = match format {
        ExportFormat::Csv => to_csv(&result).into_bytes(),
        ExportFormat::Xlsx => to_xlsx(&result).map_err(DbError::new)?,
    };
    std::fs::write(path, bytes)
        .map_err(|e| DbError::new(format!("gagal menulis {}: {e}", path.display())))?;
    Ok((result.row_count(), result.truncated))
}

fn import_csv(
    client: &mut dyn DbClient,
    owner: &str,
    name: &str,
    path: &std::path::Path,
) -> Result<u64, DbError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| DbError::new(format!("gagal membaca {}: {e}", path.display())))?;
    let csv = parse_csv(&text).map_err(DbError::new)?;
    let table_columns = client.describe_table(owner, name)?;
    let columns = map_columns(&csv.headers, &table_columns).map_err(DbError::new)?;
    client.insert_rows(owner, name, &columns, &csv.rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> DemoClient {
        let mut client = DemoClient::new();
        client
            .connect(
                &ConnectionProfile {
                    name: "demo".into(),
                    username: "hr".into(),
                    connect_string: "demo".into(),
                },
                "",
            )
            .unwrap();
        client
    }

    #[test]
    fn export_then_import_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("gpui-like-toad-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("departments.csv");
        let mut client = demo();
        let (rows, truncated) =
            export_table(&mut client, "HR", "DEPARTMENTS", &path, ExportFormat::Csv).unwrap();
        assert_eq!(rows, 4);
        assert!(!truncated);
        let inserted = import_csv(&mut client, "HR", "DEPARTMENTS", &path).unwrap();
        assert_eq!(inserted, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_rejects_unknown_columns() {
        let dir = std::env::temp_dir().join(format!("gpui-like-toad-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.csv");
        std::fs::write(&path, "DEPARTMENT_ID,NOPE\n1,2\n").unwrap();
        let err = import_csv(&mut demo(), "HR", "DEPARTMENTS", &path).unwrap_err();
        assert!(err.to_string().contains("NOPE"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
