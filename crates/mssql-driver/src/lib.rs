//! SQL Server implementation of [`ora_core::DbClient`], shipped as the
//! `kilat-ext-mssql` extension. It speaks TDS with Tiberius, over an async
//! runtime that lives inside this program only: the app's side of the
//! protocol stays synchronous.
//!
//! A profile's `connect_string` is `host:port/database`; the login is a SQL
//! Server login (user and password). The connection is encrypted and the
//! server's certificate is checked, unless the profile says to trust it.
//!
//! One session reads every database of the server: tables are named
//! `[database].[schema].[table]`, so each data tab can stay bound to the
//! database it was opened in whatever the explorer shows now. The paging
//! SQL (`OFFSET ... FETCH`, which needs an `ORDER BY`) is built in
//! `ora_core::dialect::MssqlDialect`, the same code the app uses for its
//! SQL preview; the order is the table's primary key (or a unique index).
//!
//! Cancelling a running query is deliberately not offered: Tiberius stops
//! the query on the server but then fails to read the server's
//! acknowledgement and the next query on that session returns no rows. The
//! app's "Stop waiting" ends this program instead.

mod catalog;
mod conv;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use ora_core::dialect::{rewrite_binds, BindStyle};
use ora_core::filter::{select_sql_in, TableTarget};
use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, QueryResult as Rows, RowCollector, SchemaObject, TableFilter, TablePage,
};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, QueryItem, ToSql};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use conv::{cell, db_err, first_word, type_label};

type Conn = Client<Compat<TcpStream>>;

/// Time allowed to reach the server and log in.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct MssqlClient {
    rt: Runtime,
    conn: Option<Conn>,
    /// A transaction is open (pending changes).
    in_tx: bool,
    /// Ordering keys read so far, by (database, schema, table).
    keys: HashMap<(String, String, String), Vec<String>>,
}

impl Default for MssqlClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MssqlClient {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the async runtime starts");
        MssqlClient {
            rt,
            conn: None,
            in_tx: false,
            keys: HashMap::new(),
        }
    }

    /// Run `sql` (with `@P1`... binds, if any) and collect its first result
    /// set under `limits`. Statements after the first result set are read
    /// and dropped, so the session is ready for the next call.
    fn run(&mut self, sql: &str, binds: &[String], limits: FetchLimits) -> Result<Rows, DbError> {
        let started = Instant::now();
        let Self { rt, conn, .. } = self;
        let conn = conn.as_mut().ok_or_else(|| DbError::new("not connected"))?;
        rt.block_on(async {
            let mut rows = RowCollector::started_at(limits, started);
            let params: Vec<&dyn ToSql> = binds.iter().map(|b| b as &dyn ToSql).collect();
            let mut stream = if binds.is_empty() {
                conn.simple_query(sql).await
            } else {
                conn.query(sql, &params).await
            }
            .map_err(db_err)?;
            rows.start_fetch();
            let mut columns: Option<Vec<ColumnInfo>> = None;
            let mut done = false;
            while let Some(item) = stream.try_next().await.map_err(db_err)? {
                match item {
                    QueryItem::Metadata(meta) => {
                        if columns.is_some() {
                            // A second result set: not shown, still read.
                            done = true;
                        } else {
                            columns = Some(
                                meta.columns()
                                    .iter()
                                    .map(|c| ColumnInfo {
                                        name: c.name().to_string(),
                                        data_type: type_label(c.column_type()).to_string(),
                                    })
                                    .collect(),
                            );
                        }
                    }
                    QueryItem::Row(row) => {
                        if done || rows.is_full() {
                            continue;
                        }
                        let cells = row.into_iter().map(|d| cell(d, &rows)).collect();
                        let _ = rows.push(cells);
                    }
                }
            }
            Ok(rows.finish(columns.unwrap_or_default()))
        })
    }

    /// An editor query: like [`Self::read`], with the server told to stop
    /// one row past the row limit, so a huge table is not read to the end
    /// and the session stays usable.
    fn read_capped(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<Rows, DbError> {
        self.batch(&format!(
            "SET ROWCOUNT {}",
            limits.max_rows.saturating_add(1)
        ))?;
        let result = self.read(sql, binds, limits);
        // Always lifted again, or the next statement would be capped too.
        let lifted = self.raw("SET ROWCOUNT 0");
        let result = result?;
        lifted?;
        Ok(result)
    }

    /// A read: bind spelling converted and trailing `;` or `/` removed.
    fn read(&mut self, sql: &str, binds: &[String], limits: FetchLimits) -> Result<Rows, DbError> {
        let rewritten =
            rewrite_binds(clean_statement(sql), binds, BindStyle::AtP).map_err(DbError::new)?;
        self.run(&rewritten.sql, &rewritten.binds, limits)
    }

    /// Text rows of a catalog query: every cell as text, `None` for NULL.
    pub(crate) fn text_rows(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, DbError> {
        let result = self.run(sql, &[], FetchLimits::rows(100_000))?;
        Ok(result
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| match v {
                        ora_core::Value::Null => None,
                        other => Some(other.display()),
                    })
                    .collect()
            })
            .collect())
    }

    /// The database a request names; `None` is the session's current one.
    pub(crate) fn database_of(&mut self, database: Option<&str>) -> Result<String, DbError> {
        if let Some(db) = database.filter(|d| !d.is_empty()) {
            return Ok(db.to_string());
        }
        self.text_rows("SELECT DB_NAME()")?
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next().flatten())
            .ok_or_else(|| DbError::new("The server did not name the current database."))
    }

    fn begin_if_needed(&mut self) -> Result<(), DbError> {
        if !self.in_tx {
            self.raw("BEGIN TRANSACTION")?;
            self.in_tx = true;
        }
        Ok(())
    }

    /// A statement with no result rows and no binds, sent as a plain batch
    /// (not through `sp_executesql`, so it may open or close a
    /// transaction or change a session setting).
    fn raw(&mut self, sql: &str) -> Result<(), DbError> {
        let Self { rt, conn, .. } = self;
        let conn = conn.as_mut().ok_or_else(|| DbError::new("not connected"))?;
        rt.block_on(async { conn.simple_query(sql).await?.into_results().await })
            .map(|_| ())
            .map_err(db_err)
    }

    /// A statement with no binds that changes data or the schema, sent as
    /// a plain batch so that what it creates (temporary tables, variables)
    /// lives as long as the session. Returns the rows affected: read with
    /// a closing `SELECT @@ROWCOUNT` for the statements that have a count.
    fn batch(&mut self, sql: &str) -> Result<u64, DbError> {
        let counted = matches!(
            first_word(sql).as_str(),
            "insert" | "update" | "delete" | "merge"
        );
        let Self { rt, conn, .. } = self;
        let conn = conn.as_mut().ok_or_else(|| DbError::new("not connected"))?;
        if !counted {
            return rt
                .block_on(async { conn.simple_query(sql).await?.into_results().await })
                .map(|_| 0)
                .map_err(db_err);
        }
        let text = format!("{sql}\n;SELECT @@ROWCOUNT");
        let sets = rt
            .block_on(async { conn.simple_query(text).await?.into_results().await })
            .map_err(db_err)?;
        Ok(sets
            .last()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get::<i32, _>(0))
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0))
    }

    /// A statement with `@P1`... binds that changes data; its affected rows.
    fn write(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        self.begin_if_needed()?;
        let Self { rt, conn, .. } = self;
        let conn = conn.as_mut().ok_or_else(|| DbError::new("not connected"))?;
        let params: Vec<&dyn ToSql> = binds.iter().map(|b| b as &dyn ToSql).collect();
        rt.block_on(async { conn.execute(sql, &params).await.map(|r| r.total()) })
            .map_err(db_err)
    }

    /// The columns a table is ordered by when paged: its primary key, else
    /// a unique index; empty when it has neither.
    fn order_key(
        &mut self,
        database: &str,
        owner: &str,
        table: &str,
    ) -> Result<Vec<String>, DbError> {
        let id = (database.to_string(), owner.to_string(), table.to_string());
        if let Some(key) = self.keys.get(&id) {
            return Ok(key.clone());
        }
        let key = catalog::order_key(self, database, owner, table)?;
        self.keys.insert(id, key.clone());
        Ok(key)
    }
}

impl DbClient for MssqlClient {
    fn kind(&self) -> DbKind {
        DbKind::Mssql
    }

    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        let Some((host, port, database)) =
            ora_core::model::parse_easy_connect(&profile.connect_string)
        else {
            return Err(DbError::new(
                "Enter the host, port and database of the SQL Server.",
            ));
        };
        let port: u16 = if port.is_empty() {
            1433
        } else {
            port.parse()
                .map_err(|_| DbError::new(format!("'{port}' is not a port number.")))?
        };
        // The TLS library needs a crypto provider chosen once per process;
        // another one may already be installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = Config::new();
        config.host(&host);
        config.port(port);
        if !database.is_empty() {
            config.database(&database);
        }
        config.authentication(AuthMethod::sql_server(profile.username.trim(), password));
        config.application_name("Kilat");
        config.encryption(EncryptionLevel::Required);
        if profile.access.trust_server_certificate {
            config.trust_cert();
        }
        // No client-side limit on how long a statement runs: the app's
        // "Stop waiting" ends it.
        config.command_timeout(None);
        let addr = config.get_addr();
        let Self { rt, .. } = self;
        let conn = rt.block_on(async {
            let connect = async {
                let tcp = TcpStream::connect(addr)
                    .await
                    .map_err(|e| DbError::new(format!("SQL Server: {e}")))?;
                tcp.set_nodelay(true).ok();
                Client::connect(config, tcp.compat_write())
                    .await
                    .map_err(db_err)
            };
            tokio::time::timeout(CONNECT_TIMEOUT, connect)
                .await
                .map_err(|_| DbError::new("SQL Server: the connection timed out."))?
        })?;
        self.conn = Some(conn);
        self.in_tx = false;
        self.keys.clear();
        // The filter bar quotes column names with double quotes.
        self.raw("SET QUOTED_IDENTIFIER ON")?;
        let row = self.text_rows(
            "SELECT SUSER_SNAME(), DB_NAME(), \
             CONCAT('SQL Server ', CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(50)))",
        )?;
        let row = row.into_iter().next().unwrap_or_default();
        let at = |ix: usize| row.get(ix).cloned().flatten().unwrap_or_default();
        Ok(DbInfo {
            user: at(0),
            database: at(1),
            version: at(2),
        })
    }

    fn disconnect(&mut self) {
        // Dropping the connection closes the session and rolls back.
        self.conn = None;
        self.in_tx = false;
        self.keys.clear();
    }

    fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    fn ping(&self) -> Result<(), DbError> {
        if self.is_connected() {
            Ok(())
        } else {
            Err(DbError::new("not connected"))
        }
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<Rows, DbError> {
        self.read_capped(sql, &[], limits)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<Rows, DbError> {
        self.read_capped(sql, binds, limits)
    }

    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<Rows, DbError> {
        self.query_table_in(None, owner, table, filter, page)
    }

    /// One page of `[database].[owner].[table]`, ordered by its key. The
    /// fetch is one statement with the database in its name, so it cannot
    /// run against another database whatever the session's current one is.
    fn query_table_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<Rows, DbError> {
        let database = self.database_of(database)?;
        let order = self.order_key(&database, owner, table)?;
        // Rows have no id to edit by on SQL Server.
        let page = page.with_rowid(false);
        let query = select_sql_in(
            self.dialect(),
            &TableTarget::new(Some(&database), owner, table),
            filter,
            page,
            "NULL",
            &order,
        );
        let mut result = self.read(&query.sql, &query.binds, FetchLimits::rows(page.limit))?;
        result.order_by = order;
        Ok(result)
    }

    fn count_table_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
        filter: &TableFilter,
    ) -> Result<u64, DbError> {
        let database = self.database_of(database)?;
        let query = ora_core::filter::count_sql_in(
            self.dialect(),
            &TableTarget::new(Some(&database), owner, table),
            filter,
        );
        let result = self.read(&query.sql, &query.binds, FetchLimits::rows(1))?;
        ora_core::client::count_from_result(&result)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        // A script may change keys: read them again afterwards.
        self.keys.clear();
        let sql = clean_statement(sql);
        if first_word(sql).is_empty() {
            return Err(DbError::new("empty statement"));
        }
        self.begin_if_needed()?;
        self.batch(sql)
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        if binds.is_empty() {
            return self.execute(sql);
        }
        let rewritten =
            rewrite_binds(clean_statement(sql), binds, BindStyle::AtP).map_err(DbError::new)?;
        self.write(&rewritten.sql, &rewritten.binds)
    }

    fn insert_rows(
        &mut self,
        _owner: &str,
        _table: &str,
        _columns: &[String],
        _rows: &[Vec<String>],
    ) -> Result<u64, DbError> {
        Err(DbError::new(
            "Importing rows into SQL Server is not supported yet.",
        ))
    }

    fn commit(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.raw("COMMIT TRANSACTION")?;
            self.in_tx = false;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.raw("ROLLBACK TRANSACTION")?;
            self.in_tx = false;
        }
        Ok(())
    }

    /// The databases of the server this login can open: the explorer's
    /// dropdown. (SQL Server has no level above them that people browse.)
    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        catalog::list_databases(self)
    }

    /// Objects of database `owner`, every schema of it; each object's own
    /// `owner` is its schema.
    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        catalog::list_objects(self, owner)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        self.object_ddl_in(None, owner, name, kind, false)
    }

    fn object_ddl_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
        _show_storage: bool,
    ) -> Result<String, DbError> {
        let database = self.database_of(database)?;
        catalog::object_ddl(self, &database, owner, name, kind)
    }

    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError> {
        self.describe_table_in(None, owner, name)
    }

    fn describe_table_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        name: &str,
    ) -> Result<Vec<ColumnDef>, DbError> {
        let database = self.database_of(database)?;
        catalog::describe_table(self, &database, owner, name)
    }

    fn catalog_objects(
        &mut self,
        owner: &str,
    ) -> Result<Vec<ora_core::completion::CatalogObject>, DbError> {
        catalog::catalog_objects(self, owner)
    }

    fn list_indexes(&mut self, owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        self.list_indexes_in(None, owner, table)
    }

    fn list_indexes_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
    ) -> Result<Vec<IndexDef>, DbError> {
        let database = self.database_of(database)?;
        catalog::list_indexes(self, &database, owner, table)
    }

    fn explain_plan(&mut self, _sql: &str) -> Result<Vec<String>, DbError> {
        Err(DbError::new(
            "Explain plan is not supported for SQL Server yet.",
        ))
    }
}
