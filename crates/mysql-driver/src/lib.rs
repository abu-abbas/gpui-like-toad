//! MySQL and MariaDB implementation of [`ora_core::DbClient`], shipped as
//! the `kilat-ext-mysql` extension.
//!
//! A profile's `connect_string` is `host:port/database`. Reads use the text
//! protocol, so every type arrives as the server writes it. A plain
//! `SELECT` gets a `LIMIT` one past the row limit, which tells whether
//! more rows follow without reading them.
//!
//! Changes stay pending until Commit, as in the other drivers: the first
//! statement that writes starts a transaction. MySQL rolls back only the
//! failed statement, so no savepoints are needed. DDL commits on its own
//! (as it does in every MySQL session).

mod catalog;
mod conv;
mod rowkey;
mod tls;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mysql::consts::CapabilityFlags;
use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder, Params, QueryResult, Value as My};
use ora_core::dialect::{rewrite_binds, BindStyle};
use ora_core::model::parse_easy_connect;
use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, QueryResult as Rows, RowCollector, SchemaObject, TableFilter, TablePage,
    Value,
};

use conv::{can_append_limit, cell, db_err, first_word, type_label};

#[derive(Default)]
pub struct MysqlClient {
    conn: Option<Conn>,
    /// A transaction is open (pending changes).
    in_tx: bool,
    /// The server is MariaDB (a few catalog columns differ).
    mariadb: bool,
    /// Primary keys read so far, by (schema, table).
    keys: HashMap<(String, String), Vec<rowkey::KeyColumn>>,
}

impl MysqlClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn conn(&mut self) -> Result<&mut Conn, DbError> {
        self.conn
            .as_mut()
            .ok_or_else(|| DbError::new("not connected"))
    }

    /// The primary key of a table, read once per connection.
    fn key_of(&mut self, owner: &str, table: &str) -> Result<Vec<rowkey::KeyColumn>, DbError> {
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
        let rows: Vec<mysql::Row> = self.conn()?.query(sql).map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                r.unwrap()
                    .into_iter()
                    .map(|v| match v {
                        My::NULL => None,
                        My::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                        My::Int(i) => Some(i.to_string()),
                        My::UInt(u) => Some(u.to_string()),
                        other => Some(other.as_sql(true)),
                    })
                    .collect()
            })
            .collect())
    }

    pub(crate) fn is_mariadb(&self) -> bool {
        self.mariadb
    }

    fn begin_if_needed(&mut self) -> Result<(), DbError> {
        if !self.in_tx {
            self.conn()?
                .query_drop("START TRANSACTION")
                .map_err(db_err)?;
            self.in_tx = true;
        }
        Ok(())
    }

    /// Run `sql` (with `?` binds, if any) and collect its first result.
    fn run(&mut self, sql: &str, binds: &[String], limits: FetchLimits) -> Result<Rows, DbError> {
        let started = Instant::now();
        let limited;
        let sql = if can_append_limit(sql) {
            limited = format!("{sql}\nLIMIT {}", limits.max_rows.saturating_add(1));
            limited.as_str()
        } else {
            sql
        };
        let conn = self.conn()?;
        if binds.is_empty() {
            let res = conn.query_iter(sql).map_err(db_err)?;
            collect(res, limits, started)
        } else {
            let values: Vec<My> = binds
                .iter()
                .map(|b| My::Bytes(b.clone().into_bytes()))
                .collect();
            let res = conn
                .exec_iter(sql, Params::Positional(values))
                .map_err(db_err)?;
            collect(res, limits, started)
        }
    }

    fn read(&mut self, sql: &str, binds: &[String], limits: FetchLimits) -> Result<Rows, DbError> {
        let rewritten = rewrite_binds(clean_statement(sql), binds, BindStyle::Question)
            .map_err(DbError::new)?;
        self.run(&rewritten.sql, &rewritten.binds, limits)
    }

    /// A statement with `?` binds that changes data; its affected rows.
    fn write(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        self.begin_if_needed()?;
        let conn = self.conn()?;
        let values: Vec<My> = binds
            .iter()
            .map(|b| My::Bytes(b.clone().into_bytes()))
            .collect();
        let mut res = conn
            .exec_iter(sql, Params::Positional(values))
            .map_err(db_err)?;
        let mut total = 0;
        while let Some(set) = res.iter() {
            total += set.affected_rows();
            for row in set {
                row.map_err(db_err)?;
            }
        }
        Ok(total)
    }
}

/// Read the first result set of `res` under `limits`.
fn collect<T: mysql::prelude::Protocol>(
    mut res: QueryResult<'_, '_, '_, T>,
    limits: FetchLimits,
    started: Instant,
) -> Result<Rows, DbError> {
    let mut rows = RowCollector::started_at(limits, started);
    rows.start_fetch();
    let Some(set) = res.iter() else {
        return Ok(rows.finish(Vec::new()));
    };
    let meta: Vec<mysql::Column> = set.columns().as_ref().to_vec();
    let columns: Vec<ColumnInfo> = meta
        .iter()
        .map(|c| ColumnInfo {
            name: c.name_str().into_owned(),
            data_type: type_label(c),
        })
        .collect();
    for row in set {
        let row = row.map_err(db_err)?;
        if rows.is_full() {
            break;
        }
        let values = row
            .unwrap()
            .iter()
            .zip(&meta)
            .map(|(v, c)| cell(v, c, &rows))
            .collect();
        if !rows.push(values) {
            break;
        }
    }
    Ok(rows.finish(columns))
}

impl DbClient for MysqlClient {
    fn kind(&self) -> DbKind {
        DbKind::Mysql
    }

    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        let Some((host, port, database)) = parse_easy_connect(&profile.connect_string) else {
            return Err(DbError::new(
                "Enter the host, port and database of the MySQL server.",
            ));
        };
        let port: u16 = if port.is_empty() {
            3306
        } else {
            port.parse()
                .map_err(|_| DbError::new(format!("'{port}' is not a port number.")))?
        };
        let build = |tls: bool| {
            let mut opts = OptsBuilder::new()
                .ip_or_hostname(Some(host.clone()))
                .tcp_port(port)
                .user(Some(profile.username.trim().to_string()))
                .pass(Some(password.to_string()))
                .prefer_socket(false)
                .tcp_connect_timeout(Some(Duration::from_secs(15)))
                .additional_capabilities(CapabilityFlags::CLIENT_FOUND_ROWS)
                .ssl_opts(tls.then(tls::options));
            if !database.is_empty() {
                opts = opts.db_name(Some(database.clone()));
            }
            if profile.access.read_only {
                opts = opts.init(vec!["SET SESSION TRANSACTION READ ONLY"]);
            }
            opts
        };
        let mut conn = match Conn::new(build(true)) {
            Ok(c) => c,
            // Only a server without TLS gets a second try: a wrong
            // password must not be sent twice.
            Err(e) if tls::is_tls_unavailable(&e) => Conn::new(build(false)).map_err(db_err)?,
            Err(e) => return Err(db_err(e)),
        };
        let row: Option<(String, Option<String>, String)> = conn
            .query_first("SELECT CURRENT_USER(), DATABASE(), VERSION()")
            .map_err(db_err)?;
        let (user, db, version) = row.unwrap_or_default();
        self.mariadb = version.to_ascii_lowercase().contains("mariadb");
        let info = DbInfo {
            user,
            database: db.unwrap_or_default(),
            version: if self.mariadb {
                format!("MariaDB {}", version.split('-').next().unwrap_or(&version))
            } else {
                format!("MySQL {version}")
            },
        };
        self.conn = Some(conn);
        self.in_tx = false;
        self.keys.clear();
        Ok(info)
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
        if self.conn.is_some() {
            Ok(())
        } else {
            Err(DbError::new("not connected"))
        }
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<Rows, DbError> {
        self.read(sql, &[], limits)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<Rows, DbError> {
        self.read(sql, binds, limits)
    }

    /// Rows are identified by the primary key; a table without one is
    /// shown without ids and cannot be edited.
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<Rows, DbError> {
        let expr = if page.rowid {
            let key = self.key_of(owner, table)?;
            if key.is_empty() {
                "NULL".to_string()
            } else {
                rowkey::id_expr(&key)
            }
        } else {
            String::new()
        };
        let query =
            ora_core::filter::select_sql_keyed(self.dialect(), owner, table, filter, page, &expr);
        // The id column is named with double quotes in the shared builder.
        let sql = query.sql.replacen(
            &format!("AS \"{}\"", ora_core::ROWID_COLUMN),
            &format!("AS `{}`", ora_core::ROWID_COLUMN),
            1,
        );
        self.read(&sql, &query.binds, FetchLimits::rows(page.limit))
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
        let Some((cond, binds)) = rowkey::matches(rowid, &key) else {
            return Err(DbError::new(
                "This table has no primary key to find the row by",
            ));
        };
        let d = self.dialect();
        let sql = format!(
            "SELECT {} FROM {} WHERE {cond}",
            d.quote_ident(column),
            d.table_ref(owner, table),
        );
        let limits = FetchLimits::full_cell(budget);
        let result = self.run(&sql, &binds, limits)?;
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
        let Some((cond, mut binds)) = rowkey::matches(rowid, &key) else {
            return Err(DbError::new(
                "This table has no primary key to find the row by",
            ));
        };
        let d = self.dialect();
        let (target, column) = (d.table_ref(owner, table), d.quote_ident(column));
        let sql = match value {
            Some(v) => {
                binds.insert(0, v.to_string());
                format!("UPDATE {target} SET {column} = ? WHERE {cond}")
            }
            None => format!("UPDATE {target} SET {column} = NULL WHERE {cond}"),
        };
        match self.write(&sql, &binds)? {
            1 => Ok(()),
            0 => Err(DbError::new("Row no longer exists or its key changed")),
            n => Err(DbError::new(format!(
                "Expected UPDATE to affect one row, but it affected {n}. Roll back the transaction."
            ))),
        }
    }

    fn delete_grid_row(&mut self, owner: &str, table: &str, rowid: &str) -> Result<(), DbError> {
        let key = self.key_of(owner, table)?;
        let Some((cond, binds)) = rowkey::matches(rowid, &key) else {
            return Err(DbError::new(
                "This table has no primary key to find the row by",
            ));
        };
        let d = self.dialect();
        let sql = format!("DELETE FROM {} WHERE {cond}", d.table_ref(owner, table));
        match self.write(&sql, &binds)? {
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
        let ddl = matches!(
            first_word(sql).as_str(),
            "create" | "alter" | "drop" | "truncate" | "rename"
        );
        self.begin_if_needed()?;
        let conn = self.conn()?;
        let mut res = conn.query_iter(sql).map_err(db_err)?;
        let mut total = 0;
        while let Some(set) = res.iter() {
            total += set.affected_rows();
            for row in set {
                row.map_err(db_err)?;
            }
        }
        drop(res);
        if ddl {
            // DDL committed whatever was pending.
            self.in_tx = false;
        }
        Ok(total)
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        if binds.is_empty() {
            return self.execute(sql);
        }
        let rewritten = rewrite_binds(clean_statement(sql), binds, BindStyle::Question)
            .map_err(DbError::new)?;
        self.write(&rewritten.sql, &rewritten.binds)
    }

    fn commit(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.conn()?.query_drop("COMMIT").map_err(db_err)?;
            self.in_tx = false;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        if self.in_tx {
            self.conn()?.query_drop("ROLLBACK").map_err(db_err)?;
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

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let sql = clean_statement(sql);
        let conn = self.conn()?;
        let mut res = conn.query_iter(format!("EXPLAIN {sql}")).map_err(db_err)?;
        let mut lines = Vec::new();
        if let Some(set) = res.iter() {
            let names: Vec<String> = set
                .columns()
                .as_ref()
                .iter()
                .map(|c| c.name_str().into_owned())
                .collect();
            lines.push(names.join(" | "));
            for row in set {
                let cells: Vec<String> = row
                    .map_err(db_err)?
                    .unwrap()
                    .into_iter()
                    .map(|v| match v {
                        My::NULL => "NULL".to_string(),
                        My::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
                        other => other.as_sql(true).trim_matches('\'').to_string(),
                    })
                    .collect();
                lines.push(cells.join(" | "));
            }
        }
        Ok(lines)
    }
}
