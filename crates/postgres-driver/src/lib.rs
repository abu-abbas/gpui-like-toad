//! PostgreSQL implementation of [`ora_core::DbClient`], shipped as the
//! `kilat-ext-postgres` extension.
//!
//! A profile's `connect_string` is `host:port/database`. Results are read
//! in PostgreSQL's text format through a server-side cursor, so a big table
//! is never pulled whole and every type (numeric, timestamps, arrays, ...)
//! arrives exactly as the server writes it. Binds travel as text and the
//! server reads them as the type it expects.
//!
//! Changes stay pending until Commit, as in the other drivers: the first
//! statement that writes opens a transaction. Every statement inside it
//! runs under a savepoint, so a failed one does not leave the transaction
//! aborted (PostgreSQL would otherwise refuse everything after it).

mod catalog;
mod conv;
mod rowkey;
mod tls;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ora_core::dialect::{rewrite_binds, BindStyle};
use ora_core::model::parse_easy_connect;
use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, QueryResult, RowCollector, SchemaObject, TableFilter, TablePage, Value,
};
use postgres::types::ToSql;
use postgres::{Client, Config, SimpleQueryMessage};

use conv::{cell, db_err, first_word, type_label, TextParam};

/// Rows asked from the cursor at a time.
const FETCH_BATCH: usize = 500;
const SAVEPOINT: &str = "kilat_sp";
const CURSOR: &str = "kilat_cur";

#[derive(Default)]
pub struct PostgresClient {
    client: Option<Client>,
    /// A transaction is open (pending changes).
    in_tx: bool,
    /// Primary keys read so far, by (schema, table).
    keys: HashMap<(String, String), Vec<catalog::KeyColumn>>,
}

impl PostgresClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&mut self) -> Result<&mut Client, DbError> {
        match self.client.as_mut() {
            Some(c) if !c.is_closed() => Ok(c),
            Some(_) => Err(DbError::new("PostgreSQL: the connection was closed")),
            None => Err(DbError::new("not connected")),
        }
    }

    /// Run `f` so that its failure leaves the session usable: under a
    /// savepoint when a transaction is open, otherwise in a transaction of
    /// its own that ends right after (reads).
    fn guarded<T>(
        &mut self,
        f: impl FnOnce(&mut Client) -> Result<T, postgres::Error>,
    ) -> Result<T, DbError> {
        let in_tx = self.in_tx;
        let client = self.client()?;
        let (open, close_ok, close_err) = if in_tx {
            (
                format!("SAVEPOINT {SAVEPOINT}"),
                format!("RELEASE SAVEPOINT {SAVEPOINT}"),
                format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}"),
            )
        } else {
            (
                "BEGIN".to_string(),
                "COMMIT".to_string(),
                "ROLLBACK".to_string(),
            )
        };
        client.batch_execute(&open).map_err(db_err)?;
        match f(client) {
            Ok(value) => {
                client.batch_execute(&close_ok).map_err(db_err)?;
                Ok(value)
            }
            Err(err) => {
                let _ = client.batch_execute(&close_err);
                Err(db_err(err))
            }
        }
    }

    /// Like [`Self::guarded`] but for a statement that changes data: the
    /// transaction stays open afterwards.
    fn guarded_write<T>(
        &mut self,
        f: impl FnOnce(&mut Client) -> Result<T, postgres::Error>,
    ) -> Result<T, DbError> {
        if !self.in_tx {
            self.client()?.batch_execute("BEGIN").map_err(db_err)?;
            self.in_tx = true;
        }
        self.guarded(f)
    }

    /// The primary key of a table, read once per connection.
    fn key_of(&mut self, owner: &str, table: &str) -> Result<Vec<catalog::KeyColumn>, DbError> {
        let id = (owner.to_string(), table.to_string());
        if let Some(key) = self.keys.get(&id) {
            return Ok(key.clone());
        }
        let key = catalog::primary_key(self, owner, table)?;
        self.keys.insert(id, key.clone());
        Ok(key)
    }

    /// Text rows of a catalog query (every value as the server writes it).
    pub(crate) fn text_rows(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
        let messages = self.guarded(|c| c.simple_query(sql))?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => {
                    Some((0..r.len()).map(|i| r.get(i).map(str::to_string)).collect())
                }
                _ => None,
            })
            .collect())
    }

    fn read(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        let rewritten =
            rewrite_binds(clean_statement(sql), binds, BindStyle::Dollar).map_err(DbError::new)?;
        let sql = rewritten.sql.as_str();
        let started = Instant::now();
        let params: Vec<TextParam<'_>> = binds.iter().map(|b| TextParam(b)).collect();
        let refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        let cursorable = matches!(
            first_word(sql).as_str(),
            "select" | "with" | "values" | "table"
        );
        let in_tx = self.in_tx;
        let client = self.client()?;
        if cursorable {
            // A cursor lives inside a transaction.
            let result = if in_tx {
                client
                    .batch_execute(&format!("SAVEPOINT {SAVEPOINT}"))
                    .map_err(db_err)?;
                let r = read_cursor(client, sql, &refs, limits, started);
                let end = if r.is_ok() {
                    format!("RELEASE SAVEPOINT {SAVEPOINT}")
                } else {
                    format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")
                };
                let _ = client.batch_execute(&end);
                r
            } else {
                client.batch_execute("BEGIN").map_err(db_err)?;
                let r = read_cursor(client, sql, &refs, limits, started);
                let _ = client.batch_execute(if r.is_ok() { "COMMIT" } else { "ROLLBACK" });
                r
            };
            match result {
                // `WITH ... INSERT ... RETURNING` cannot be a cursor: run it.
                Err(CursorError::NotCursor) => {}
                Err(CursorError::Db(e)) => return Err(db_err(e)),
                Ok(r) => return Ok(r),
            }
        }
        if !binds.is_empty() {
            return Err(DbError::new(
                "PostgreSQL: bind variables work in SELECT and WITH statements only",
            ));
        }
        // SHOW, EXPLAIN and the like: small results, read whole as text.
        let messages = self.guarded(|c| c.simple_query(sql))?;
        Ok(simple_result(messages, limits, started))
    }
}

enum CursorError {
    NotCursor,
    Db(postgres::Error),
}

/// Open a cursor over `sql`, fetch under `limits`, close it. Must run
/// inside a transaction.
fn read_cursor(
    client: &mut Client,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
    limits: FetchLimits,
    started: Instant,
) -> Result<QueryResult, CursorError> {
    let stmt = client.prepare(sql).map_err(CursorError::Db)?;
    let columns: Vec<ColumnInfo> = stmt
        .columns()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            data_type: type_label(c.type_()),
        })
        .collect();
    let types: Vec<_> = stmt.columns().iter().map(|c| c.type_().clone()).collect();
    if let Err(e) = client.execute(
        &format!("DECLARE {CURSOR} NO SCROLL CURSOR FOR {sql}"),
        params,
    ) {
        let not_cursor = e.as_db_error().is_some_and(|d| {
            d.message().contains("DECLARE CURSOR") || d.message().contains("cursor")
        });
        return Err(if not_cursor {
            CursorError::NotCursor
        } else {
            CursorError::Db(e)
        });
    }
    let mut rows = RowCollector::started_at(limits, started);
    rows.start_fetch();
    let mut fetched = 0usize;
    'fetch: loop {
        // One row past the limit tells whether more follow.
        let want = FETCH_BATCH
            .min(limits.max_rows.saturating_add(1).saturating_sub(fetched))
            .max(1);
        let messages = client
            .simple_query(&format!("FETCH FORWARD {want} FROM {CURSOR}"))
            .map_err(CursorError::Db)?;
        let mut got = 0usize;
        for message in messages {
            let SimpleQueryMessage::Row(row) = message else {
                continue;
            };
            got += 1;
            if rows.is_full() {
                break 'fetch;
            }
            let values = types
                .iter()
                .enumerate()
                .map(|(i, ty)| cell(row.get(i), ty, &rows))
                .collect();
            if !rows.push(values) {
                break 'fetch;
            }
        }
        fetched += got;
        if got < want {
            break;
        }
    }
    let _ = client.batch_execute(&format!("CLOSE {CURSOR}"));
    Ok(rows.finish(columns))
}

/// A result from `simple_query` messages: column names only, all text.
fn simple_result(
    messages: Vec<SimpleQueryMessage>,
    limits: FetchLimits,
    started: Instant,
) -> QueryResult {
    let mut rows = RowCollector::started_at(limits, started);
    rows.start_fetch();
    let mut columns: Vec<ColumnInfo> = Vec::new();
    for message in messages {
        let SimpleQueryMessage::Row(row) = message else {
            continue;
        };
        if columns.is_empty() {
            columns = row
                .columns()
                .iter()
                .map(|c| ColumnInfo {
                    name: c.name().to_string(),
                    data_type: String::new(),
                })
                .collect();
        }
        if rows.is_full() {
            break;
        }
        let values = (0..row.len())
            .map(|i| row.get(i).map_or(ora_core::Value::Null, |t| rows.text(t)))
            .collect();
        if !rows.push(values) {
            break;
        }
    }
    rows.finish(columns)
}

impl DbClient for PostgresClient {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        let Some((host, port, database)) = parse_easy_connect(&profile.connect_string) else {
            return Err(DbError::new(
                "Enter the host, port and database of the PostgreSQL server.",
            ));
        };
        let port: u16 = if port.is_empty() {
            5432
        } else {
            port.parse()
                .map_err(|_| DbError::new(format!("'{port}' is not a port number.")))?
        };
        let mut config = Config::new();
        config
            .host(&host)
            .port(port)
            .user(profile.username.trim())
            .password(password)
            .application_name("Kilat")
            .connect_timeout(Duration::from_secs(15))
            .ssl_mode(postgres::config::SslMode::Prefer);
        if !database.is_empty() {
            config.dbname(&database);
        }
        if profile.access.read_only {
            config.options("-c default_transaction_read_only=on");
        }
        let tls = tls::connector().map_err(DbError::new)?;
        let mut client = config.connect(tls).map_err(db_err)?;
        let row = client
            .query_one(
                "SELECT current_user::text, current_database()::text, \
                 current_setting('server_version')::text",
                &[],
            )
            .map_err(db_err)?;
        let info = DbInfo {
            user: row.get(0),
            database: row.get(1),
            version: format!("PostgreSQL {}", row.get::<_, String>(2)),
        };
        self.client = Some(client);
        self.in_tx = false;
        Ok(info)
    }

    fn disconnect(&mut self) {
        // Dropping the client closes the session and rolls back.
        self.client = None;
        self.in_tx = false;
        self.keys.clear();
    }

    fn is_connected(&self) -> bool {
        self.client.as_ref().is_some_and(|c| !c.is_closed())
    }

    fn ping(&self) -> Result<(), DbError> {
        // `simple_query` needs `&mut`; the closed flag is what `&self` can see.
        if self.is_connected() {
            Ok(())
        } else {
            Err(DbError::new("PostgreSQL: the connection was closed"))
        }
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<QueryResult, DbError> {
        self.read(sql, &[], limits)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        self.read(sql, binds, limits)
    }

    /// Rows are identified by the primary key when the table has one,
    /// otherwise by `ctid` (see `rowkey`).
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        let expr = if page.rowid {
            let key = self.key_of(owner, table)?;
            if key.is_empty() {
                self.dialect().row_id_expr().to_string()
            } else {
                rowkey::id_expr(&key)
            }
        } else {
            String::new()
        };
        let query =
            ora_core::filter::select_sql_keyed(self.dialect(), owner, table, filter, page, &expr);
        self.read(&query.sql, &query.binds, FetchLimits::rows(page.limit))
    }

    fn fetch_cell(
        &mut self,
        owner: &str,
        table: &str,
        column: &str,
        rowid: &str,
        budget: usize,
    ) -> Result<Value, DbError> {
        let key = self.key_of(owner, table)?;
        let d = self.dialect();
        let sql = format!(
            "SELECT {} FROM {} WHERE {}",
            d.quote_ident(column),
            d.table_ref(owner, table),
            rowkey::matches(d, rowid, &key, 1)
        );
        let limits = FetchLimits::full_cell(budget);
        let result = self.read(&sql, &[rowid.to_string()], limits)?;
        match result
            .rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
        {
            Some(Value::Clipped(c)) => Err(DbError::new(format!(
                "The value is {}, larger than the {} that can be loaded into the grid.",
                c.describe_len(),
                ora_core::fetch::human_bytes(limits.max_cell_bytes)
            ))),
            Some(value) if value.heap_bytes() > budget => Err(DbError::new(format!(
                "The value needs {}, more than the {} left in the open results.",
                ora_core::fetch::human_bytes(value.heap_bytes()),
                ora_core::fetch::human_bytes(budget)
            ))),
            Some(value) => Ok(value),
            None => Err(DbError::new("Row no longer exists or its key changed")),
        }
    }

    fn update_cell(
        &mut self,
        owner: &str,
        table: &str,
        column: &str,
        rowid: &str,
        value: Option<&str>,
    ) -> Result<(), DbError> {
        let key = self.key_of(owner, table)?;
        let d = self.dialect();
        let (target, column) = (d.table_ref(owner, table), d.quote_ident(column));
        let (sql, binds) = match value {
            Some(v) => (
                format!(
                    "UPDATE {target} SET {column} = :1 WHERE {}",
                    rowkey::matches(d, rowid, &key, 2)
                ),
                vec![v.to_string(), rowid.to_string()],
            ),
            None => (
                format!(
                    "UPDATE {target} SET {column} = NULL WHERE {}",
                    rowkey::matches(d, rowid, &key, 1)
                ),
                vec![rowid.to_string()],
            ),
        };
        match self.execute_binds(&sql, &binds)? {
            1 => Ok(()),
            0 => Err(DbError::new("Row no longer exists or its key changed")),
            n => Err(DbError::new(format!(
                "Expected UPDATE to affect one row, but it affected {n}. Roll back the transaction."
            ))),
        }
    }

    fn delete_grid_row(&mut self, owner: &str, table: &str, rowid: &str) -> Result<(), DbError> {
        let key = self.key_of(owner, table)?;
        let d = self.dialect();
        let sql = format!(
            "DELETE FROM {} WHERE {}",
            d.table_ref(owner, table),
            rowkey::matches(d, rowid, &key, 1)
        );
        match self.execute_binds(&sql, &[rowid.to_string()])? {
            1 => Ok(()),
            n => Err(DbError::new(format!(
                "Expected to delete one row, affected {n}"
            ))),
        }
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        // A script may change keys: read them again afterwards.
        self.keys.clear();
        let sql = clean_statement(sql);
        // Statements without binds may be several commands (a script).
        let messages = self.guarded_write(|c| c.simple_query(sql))?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::CommandComplete(n) => Some(n),
                _ => None,
            })
            .sum())
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        if binds.is_empty() {
            return self.execute(sql);
        }
        let rewritten =
            rewrite_binds(clean_statement(sql), binds, BindStyle::Dollar).map_err(DbError::new)?;
        let sql = rewritten.sql.as_str();
        let params: Vec<TextParam<'_>> = binds.iter().map(|b| TextParam(b)).collect();
        let refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        self.guarded_write(|c| c.execute(sql, &refs))
    }

    fn commit(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.client()?.batch_execute("COMMIT").map_err(db_err)?;
            self.in_tx = false;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.client()?.batch_execute("ROLLBACK").map_err(db_err)?;
            self.in_tx = false;
        }
        Ok(())
    }

    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        catalog::list_objects(self, owner)
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        catalog::list_schemas(self)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        catalog::object_ddl(self, owner, name, kind)
    }

    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError> {
        catalog::describe_table(self, owner, name)
    }

    fn catalog_objects(
        &mut self,
        owner: &str,
    ) -> Result<Vec<ora_core::completion::CatalogObject>, DbError> {
        catalog::catalog_objects(self, owner)
    }

    fn list_indexes(&mut self, owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        catalog::list_indexes(self, owner, table)
    }

    fn list_constraints(
        &mut self,
        owner: &str,
        table: &str,
    ) -> Result<Vec<ora_core::ConstraintDef>, DbError> {
        catalog::list_constraints(self, owner, table)
    }

    fn trigger_scripts(&mut self, owner: &str, table: &str) -> Result<Vec<String>, DbError> {
        catalog::trigger_scripts(self, owner, table)
    }

    fn explain_analyze(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let sql = clean_statement(sql);
        if !ora_core::sql::is_single_statement(sql) {
            return Err(DbError::new("EXPLAIN ANALYZE runs one statement at a time"));
        }
        // DDL and utility commands are not offered: only statements the
        // rollback below reliably undoes.
        if !matches!(
            first_word(sql).as_str(),
            "select" | "with" | "values" | "table" | "insert" | "update" | "delete" | "merge"
        ) {
            return Err(DbError::new(
                "EXPLAIN ANALYZE works on SELECT, INSERT, UPDATE, DELETE and MERGE only",
            ));
        }
        let in_tx = self.in_tx;
        let client = self.client()?;
        // A savepoint inside the caller's open transaction (so their pending
        // changes survive), else a transaction of its own. Either way the
        // statement is rolled back, whether it worked or not.
        let (open, undo) = if in_tx {
            (
                format!("SAVEPOINT {SAVEPOINT}_x"),
                format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}_x; RELEASE SAVEPOINT {SAVEPOINT}_x"),
            )
        } else {
            ("BEGIN".to_string(), "ROLLBACK".to_string())
        };
        client.batch_execute(&open).map_err(db_err)?;
        let result = client.simple_query(&format!("EXPLAIN (ANALYZE, BUFFERS) {sql}"));
        let undone = client.batch_execute(&undo);
        let messages = result.map_err(db_err)?;
        undone.map_err(|e| {
            DbError::new(format!(
                "The plan ran but the rollback failed, check your data: {e}"
            ))
        })?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => r.get(0).map(str::to_string),
                _ => None,
            })
            .collect())
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let sql = clean_statement(sql);
        let rows = self.text_rows(&format!("EXPLAIN {sql}"))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }
}
