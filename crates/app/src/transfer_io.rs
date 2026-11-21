//! [`TransferIo`] over a connection's worker: every call is one job on
//! that connection's own thread, so the transfer shares the worker (and its
//! Read Only / production guards) with the rest of the app. The transfer
//! thread blocks on the replies; the UI thread is never involved.

use std::collections::HashSet;

use ora_core::transfer::run::TransferIo;
use ora_core::{ColumnDef, DbError, DbKind, IndexDef, ObjectKind, QueryResult};

use crate::worker::{DbHandle, Job, Response};

pub struct WorkerIo {
    db: DbHandle,
    kind: DbKind,
    /// The user confirmed the production prompt for this target.
    confirmed: bool,
    /// Tables of the schema, read once.
    tables: Option<(String, HashSet<String>)>,
}

impl WorkerIo {
    pub fn new(db: DbHandle, kind: DbKind, confirmed: bool) -> Self {
        WorkerIo {
            db,
            kind,
            confirmed,
            tables: None,
        }
    }

    fn call(&self, job: Job) -> Result<Response, DbError> {
        let rx = if self.confirmed {
            self.db.request_confirmed(job)
        } else {
            self.db.request(job)
        };
        match rx.recv() {
            Ok(Response::Failed(message)) => Err(DbError::new(message)),
            Ok(Response::ConnectionLost(reason)) => {
                Err(DbError::new(format!("connection lost: {reason}")))
            }
            Ok(other) => Ok(other),
            Err(_) => Err(DbError::new("the database worker stopped")),
        }
    }
}

fn unexpected() -> DbError {
    DbError::new("unexpected reply from the database worker")
}

impl TransferIo for WorkerIo {
    fn kind(&self) -> DbKind {
        self.kind
    }

    fn describe(&mut self, schema: &str, table: &str) -> Result<Vec<ColumnDef>, DbError> {
        match self.call(Job::Describe {
            owner: schema.to_string(),
            name: table.to_string(),
        })? {
            Response::Described(columns) => Ok(columns),
            _ => Err(unexpected()),
        }
    }

    fn indexes(&mut self, schema: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        match self.call(Job::Indexes {
            owner: schema.to_string(),
            name: table.to_string(),
        })? {
            Response::Indexes(indexes) => Ok(indexes),
            _ => Err(unexpected()),
        }
    }

    fn constraints(
        &mut self,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ora_core::ConstraintDef>, DbError> {
        match self.call(Job::Constraints {
            owner: schema.to_string(),
            name: table.to_string(),
        })? {
            Response::Constraints(constraints) => Ok(constraints),
            _ => Err(unexpected()),
        }
    }

    fn table_exists(&mut self, schema: &str, table: &str) -> Result<bool, DbError> {
        if self.tables.as_ref().is_none_or(|(s, _)| s != schema) {
            let objects = match self.call(Job::ListObjects {
                owner: schema.to_string(),
            })? {
                Response::Objects(objects) => objects,
                _ => return Err(unexpected()),
            };
            let names = objects
                .into_iter()
                .filter(|o| o.kind == ObjectKind::Table)
                .map(|o| o.name)
                .collect();
            self.tables = Some((schema.to_string(), names));
        }
        Ok(self
            .tables
            .as_ref()
            .is_some_and(|(_, names)| names.contains(table)))
    }

    fn read(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        match self.call(Job::TransferRead {
            sql: sql.to_string(),
            max_rows,
        })? {
            Response::QueryDone(result) => Ok(result),
            _ => Err(unexpected()),
        }
    }

    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)> {
        match self
            .call(Job::TransferExec {
                statements: statements.to_vec(),
            })
            .map_err(|e| (0, e))?
        {
            Response::TransferExecuted { done, error: None } => Ok(done as u64),
            Response::TransferExecuted {
                done,
                error: Some(message),
            } => Err((done, DbError::new(message))),
            _ => Err((0, unexpected())),
        }
    }

    fn commit(&mut self) -> Result<(), DbError> {
        match self.call(Job::Commit)? {
            Response::Committed => Ok(()),
            _ => Err(unexpected()),
        }
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        match self.call(Job::Rollback)? {
            Response::RolledBack => Ok(()),
            _ => Err(unexpected()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use ora_core::credentials::SecretString;
    use ora_core::transfer::{run_transfer, TableSpec, TransferOptions, TransferSpec};
    use ora_core::{ConnectionAccess, ConnectionProfile, Value};

    use super::*;
    use crate::worker::Password;

    /// A worker connected to a fresh SQLite file; `setup` statements run
    /// on it first and are committed.
    fn connect(tag: &str, setup: &[&str], read_only: bool) -> (DbHandle, std::path::PathBuf) {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let path = std::env::temp_dir().join(format!(
            "transfer-io-{tag}-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        // An empty file is a valid, empty SQLite database.
        std::fs::File::create(&path).unwrap();
        let db = DbHandle::spawn();
        let reply = db
            .request(Job::Connect {
                profile: ConnectionProfile {
                    name: tag.into(),
                    kind: DbKind::Sqlite,
                    username: String::new(),
                    connect_string: path.to_string_lossy().into_owned(),
                    access: ConnectionAccess::default(),
                },
                password: Password::Typed(SecretString::from(String::new())),
                demo: false,
            })
            .recv()
            .unwrap();
        assert!(matches!(reply, Response::Connected(_)));
        for sql in setup {
            let reply = db
                .request(Job::Run {
                    sql: sql.to_string(),
                })
                .recv()
                .unwrap();
            assert!(!matches!(reply, Response::Failed(_)), "{sql}");
        }
        assert!(matches!(
            db.request(Job::Commit).recv().unwrap(),
            Response::Committed
        ));
        if read_only {
            // Reconnect the same file with the Read Only flag.
            let reply = db
                .request(Job::Connect {
                    profile: ConnectionProfile {
                        name: tag.into(),
                        kind: DbKind::Sqlite,
                        username: String::new(),
                        connect_string: path.to_string_lossy().into_owned(),
                        access: ConnectionAccess {
                            read_only: true,
                            ..Default::default()
                        },
                    },
                    password: Password::Typed(SecretString::from(String::new())),
                    demo: false,
                })
                .recv()
                .unwrap();
            assert!(matches!(reply, Response::Connected(_)));
        }
        (db, path)
    }

    fn read(db: &DbHandle, sql: &str) -> Vec<Vec<String>> {
        match db
            .request(Job::TransferRead {
                sql: sql.to_string(),
                max_rows: 100,
            })
            .recv()
            .unwrap()
        {
            Response::QueryDone(r) => r
                .rows
                .iter()
                .map(|row| row.iter().map(Value::display).collect())
                .collect(),
            _ => panic!("read failed: {sql}"),
        }
    }

    fn spec() -> TransferSpec {
        TransferSpec {
            source_schema: "main".into(),
            target_schema: "main".into(),
            tables: vec![TableSpec::auto("t")],
            options: TransferOptions::default(),
        }
    }

    const SOURCE: [&str; 2] = [
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
        "INSERT INTO t VALUES (1, 'a'), (2, 'b''c')",
    ];

    #[test]
    fn transfer_runs_between_two_workers() {
        let (src, src_path) = connect("w-src", &SOURCE, false);
        let (dst, dst_path) = connect("w-dst", &[], false);
        let mut source = WorkerIo::new(src, DbKind::Sqlite, false);
        let mut target = WorkerIo::new(dst.clone(), DbKind::Sqlite, false);
        let summary = run_transfer(
            &mut source,
            &mut target,
            &spec(),
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(summary.ok(), "{summary:?}");
        assert_eq!(summary.rows(), 2);
        assert_eq!(read(&dst, "SELECT v FROM t WHERE id = 2"), [["b'c"]]);
        let _ = std::fs::remove_file(src_path);
        let _ = std::fs::remove_file(dst_path);
    }

    #[test]
    fn read_only_target_is_refused_by_the_worker_itself() {
        let (src, src_path) = connect("ro-src", &SOURCE, false);
        let (dst, dst_path) = connect("ro-dst", &[], true);
        let mut source = WorkerIo::new(src, DbKind::Sqlite, false);
        let mut target = WorkerIo::new(dst.clone(), DbKind::Sqlite, false);
        let summary = run_transfer(
            &mut source,
            &mut target,
            &spec(),
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(summary.failed_tables(), 1);
        let error = summary.tables[0].error.clone().unwrap();
        assert!(error.contains("Read Only"), "{error}");
        assert_eq!(read(&dst, "SELECT count(*) FROM sqlite_master"), [["0"]]);
        let _ = std::fs::remove_file(src_path);
        let _ = std::fs::remove_file(dst_path);
    }
}
