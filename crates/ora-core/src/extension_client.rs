//! [`RemoteClient`]: a [`DbClient`] that runs in an extension program. The
//! app uses it like any other driver; every method becomes one request of
//! the extension protocol (see `docs/extension-protocol.md`).
//!
//! The program is started by `connect` and stopped by `disconnect` (or when
//! the client is dropped), so an extension that is not in use is not
//! running. Everything it sends is untrusted: row counts are checked
//! against the limits the app asked for, and a result of the wrong shape
//! kills the program.

use crate::client::{count_from_result, DbClient, DbError, DbInfo};
use crate::dialect::DbKind;
use crate::extension_host::{CallError, ExtProcess};
use crate::extension_wire as wire;
use crate::filter::{TableFilter, TablePage};
use crate::model::{
    ColumnDef, ConnectionProfile, ConstraintDef, IndexDef, ObjectKind, PlanNode, QueryResult,
    SchemaObject,
};
use crate::store::Json;
use crate::FetchLimits;
use std::path::PathBuf;
use std::time::Duration;

/// How long the program may take to answer the handshake.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest silence tolerated while a request runs. A long query that
/// sends nothing for this long is cut off; the next call reconnects.
const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub struct RemoteClient {
    program: PathBuf,
    ext_id: String,
    kind: DbKind,
    app_version: String,
    process: Option<ExtProcess>,
    connected: bool,
    call_timeout: Duration,
}

impl RemoteClient {
    /// A client for the extension installed at `program` under `ext_id`
    /// (the id its handshake must report), speaking `kind`'s SQL.
    pub fn new(program: PathBuf, ext_id: &str, kind: DbKind, app_version: &str) -> Self {
        Self {
            program,
            ext_id: ext_id.to_string(),
            kind,
            app_version: app_version.to_string(),
            process: None,
            connected: false,
            call_timeout: CALL_TIMEOUT,
        }
    }

    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// What the extension wrote to stderr lately, for error reports.
    pub fn stderr_tail(&self) -> String {
        self.process
            .as_ref()
            .map(ExtProcess::stderr_tail)
            .unwrap_or_default()
    }

    fn process(&mut self) -> Result<&mut ExtProcess, DbError> {
        match self.process.as_mut() {
            Some(p) if !p.is_broken() => Ok(p),
            _ => {
                self.connected = false;
                Err(DbError::new("not connected"))
            }
        }
    }

    fn call(&mut self, method: &str, params: Json) -> Result<Json, DbError> {
        let timeout = self.call_timeout;
        let outcome = self.process()?.call(method, params, timeout);
        self.finish(outcome)
    }

    fn finish<T>(&mut self, outcome: Result<T, CallError>) -> Result<T, DbError> {
        outcome.map_err(|e| {
            if self.process.as_ref().is_none_or(ExtProcess::is_broken) {
                self.connected = false;
            }
            DbError::new(e.to_string())
        })
    }

    /// A call whose answer is a bad shape: the program is killed.
    fn bad<T>(&mut self, why: String) -> Result<T, DbError> {
        let error = match self.process.as_mut() {
            Some(p) => p.violation(&why),
            None => CallError::Protocol(why),
        };
        self.connected = false;
        Err(DbError::new(error.to_string()))
    }

    fn decode<T>(&mut self, reply: &Json, f: fn(&Json) -> Result<T, String>) -> Result<T, DbError> {
        match f(reply) {
            Ok(value) => Ok(value),
            Err(why) => self.bad(why),
        }
    }

    /// A request answered by row chunks and a closing result.
    fn call_rows(
        &mut self,
        method: &str,
        params: Json,
        limits: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        let timeout = self.call_timeout;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        let outcome = self
            .process()?
            .call_streaming(method, params, timeout, &mut |chunk| {
                let items = chunk
                    .get("rows")
                    .and_then(Json::as_arr)
                    .ok_or("a chunk has no rows")?;
                for item in items {
                    let row = wire::row_from_json(item)?;
                    bytes = bytes.saturating_add(row.iter().map(crate::Value::heap_bytes).sum());
                    rows.push(row);
                    if rows.len() > limits.max_rows {
                        return Err("more rows than the limit".to_string());
                    }
                    if bytes
                        > limits
                            .max_result_bytes
                            .saturating_add(limits.max_cell_bytes)
                    {
                        return Err("more data than the limit".to_string());
                    }
                }
                Ok(())
            });
        let head = self.finish(outcome)?;
        let mut result = match wire::result_from_json(&head) {
            Ok(r) => r,
            Err(why) => return self.bad(why),
        };
        if !result.rows.is_empty() {
            return self.bad("the closing result carries rows".to_string());
        }
        result.rows = rows;
        Ok(result)
    }
}

fn params(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// `params` with an optional `database` added. `None` leaves it out, so a
/// request for the session's own database looks exactly as it did before
/// databases were named, and an older extension never sees the field.
fn scoped(database: Option<&str>, mut pairs: Vec<(&str, Json)>) -> Json {
    if let Some(db) = database {
        pairs.insert(0, ("database", s(db)));
    }
    params(pairs)
}

fn s(v: &str) -> Json {
    Json::Str(v.to_string())
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl DbClient for RemoteClient {
    fn kind(&self) -> DbKind {
        self.kind
    }

    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        self.disconnect();
        let process = ExtProcess::start(
            &self.program,
            &self.ext_id,
            &self.app_version,
            START_TIMEOUT,
        )
        .map_err(|e| {
            DbError::new(format!(
                "Could not start the {} extension: {e}",
                self.ext_id
            ))
        })?;
        self.process = Some(process);
        let reply = self.call(
            "connect",
            params(vec![
                ("profile", wire::profile_to_json(profile)),
                ("password", s(password)),
            ]),
        );
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                self.disconnect();
                return Err(e);
            }
        };
        match wire::db_info_from_json(&reply) {
            Ok(info) => {
                self.connected = true;
                Ok(info)
            }
            Err(why) => {
                let _ = self.bad::<()>(why.clone());
                self.disconnect();
                Err(DbError::new(format!(
                    "The extension sent a bad answer: {why}"
                )))
            }
        }
    }

    fn disconnect(&mut self) {
        self.connected = false;
        if let Some(mut process) = self.process.take() {
            if !process.is_broken() {
                let _ = process.call("disconnect", Json::Obj(Vec::new()), Duration::from_secs(5));
            }
            process.stop();
        }
    }

    fn is_connected(&self) -> bool {
        self.connected && self.process.as_ref().is_some_and(|p| !p.is_broken())
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<QueryResult, DbError> {
        self.query_binds(sql, &[], limits)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        let request = params(vec![
            ("sql", s(sql)),
            ("binds", wire::strings_to_json(binds)),
            ("limits", wire::limits_to_json(&limits)),
        ]);
        self.call_rows("query", request, limits)
    }

    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        self.query_table_in(None, owner, table, filter, page)
    }

    fn query_table_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        let request = scoped(
            database,
            vec![
                ("owner", s(owner)),
                ("table", s(table)),
                ("filter", wire::filter_to_json(filter)),
                ("page", wire::page_to_json(&page)),
            ],
        );
        self.call_rows("query_table", request, FetchLimits::rows(page.limit))
    }

    /// Without a database the count goes through `query`, as before. For a
    /// named database the extension counts, so the database travels with
    /// the request.
    fn count_table_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
        filter: &TableFilter,
    ) -> Result<u64, DbError> {
        let Some(database) = database else {
            let query = crate::filter::count_sql(self.dialect(), owner, table, filter);
            let result = self.query_binds(&query.sql, &query.binds, FetchLimits::rows(1))?;
            return count_from_result(&result);
        };
        let request = scoped(
            Some(database),
            vec![
                ("owner", s(owner)),
                ("table", s(table)),
                ("filter", wire::filter_to_json(filter)),
            ],
        );
        let reply = self.call("count_table", request)?;
        match reply.get("count").and_then(Json::as_u64) {
            Some(n) => Ok(n),
            None => self.bad("count_table has no count".to_string()),
        }
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        self.execute_binds(sql, &[])
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        let reply = self.call(
            "execute",
            params(vec![
                ("sql", s(sql)),
                ("binds", wire::strings_to_json(binds)),
            ]),
        )?;
        match reply.get("affected").and_then(Json::as_u64) {
            Some(n) => Ok(n),
            None => self.bad("execute has no affected count".to_string()),
        }
    }

    fn commit(&mut self) -> Result<(), DbError> {
        self.call("commit", Json::Obj(Vec::new())).map(|_| ())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        self.call("rollback", Json::Obj(Vec::new())).map(|_| ())
    }

    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        let reply = self.call("list_objects", params(vec![("owner", s(owner))]))?;
        self.decode(&reply, wire::objects_from_json)
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        let reply = self.call("list_schemas", Json::Obj(Vec::new()))?;
        self.decode(&reply, wire::strings_from_json)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        self.object_ddl_with(owner, name, kind, false)
    }

    fn object_ddl_with(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
        show_storage: bool,
    ) -> Result<String, DbError> {
        self.object_ddl_in(None, owner, name, kind, show_storage)
    }

    fn object_ddl_in(
        &mut self,
        database: Option<&str>,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
        show_storage: bool,
    ) -> Result<String, DbError> {
        let reply = self.call(
            "object_ddl",
            scoped(
                database,
                vec![
                    ("owner", s(owner)),
                    ("name", s(name)),
                    ("kind", wire::kind_to_json(kind)),
                    ("show_storage", Json::Bool(show_storage)),
                ],
            ),
        )?;
        match reply {
            Json::Str(text) => Ok(text),
            _ => self.bad("object_ddl did not answer with text".to_string()),
        }
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
        let reply = self.call(
            "describe_table",
            scoped(database, vec![("owner", s(owner)), ("name", s(name))]),
        )?;
        self.decode(&reply, wire::columns_from_json)
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
        let reply = self.call(
            "list_indexes",
            scoped(database, vec![("owner", s(owner)), ("table", s(table))]),
        )?;
        self.decode(&reply, wire::indexes_from_json)
    }

    fn list_constraints(
        &mut self,
        owner: &str,
        table: &str,
    ) -> Result<Vec<ConstraintDef>, DbError> {
        let reply = self.call(
            "list_constraints",
            params(vec![("owner", s(owner)), ("table", s(table))]),
        )?;
        self.decode(&reply, wire::constraints_from_json)
    }

    fn trigger_scripts(&mut self, owner: &str, table: &str) -> Result<Vec<String>, DbError> {
        let reply = self.call(
            "trigger_scripts",
            params(vec![("owner", s(owner)), ("table", s(table))]),
        )?;
        self.decode(&reply, wire::strings_from_json)
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let reply = self.call("explain_plan", params(vec![("sql", s(sql))]))?;
        self.decode(&reply, wire::strings_from_json)
    }

    fn explain_plan_nodes(&mut self, sql: &str) -> Result<Vec<PlanNode>, DbError> {
        let reply = self.call("explain_plan_nodes", params(vec![("sql", s(sql))]))?;
        self.decode(&reply, wire::plan_nodes_from_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request for the session's own database looks as it did before
    /// databases were named, so older extensions never see the field.
    #[test]
    fn a_request_without_a_database_has_no_database_field() {
        let plain = scoped(None, vec![("owner", s("dbo"))]);
        assert!(plain.get("database").is_none());
        assert_eq!(plain.get("owner").and_then(Json::as_str), Some("dbo"));
        let named = scoped(Some("staging"), vec![("owner", s("dbo"))]);
        assert_eq!(
            named.get("database").and_then(Json::as_str),
            Some("staging")
        );
        assert_eq!(named.get("owner").and_then(Json::as_str), Some("dbo"));
    }
}
