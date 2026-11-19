//! Oracle thin-driver implementation of [`ora_core::DbClient`].
//!
//! Uses Oracle's official `oracledb` crate in thin mode: the driver speaks
//! the Oracle Net protocol directly, so end users need no Oracle Client /
//! Instant Client install. The crate is a pre-release (`26.0.0-beta.7` at the
//! pinned revision); its API surface used here is small and covered by the
//! driver's own guide, and the `DbClient` trait isolates the GUI from it.

use std::collections::HashMap;
use std::time::Instant;

use ora_core::completion::CatalogObject;
use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, IndexDef, ObjectKind,
    PlanNode, QueryResult, SchemaObject, Value,
};

/// Blocking Oracle client. Owned exclusively by the GUI's background DB
/// worker thread; all methods run off the UI thread.
pub struct ThinClient {
    conn: Option<oracledb::Connection>,
    read_only: bool,
}

impl ThinClient {
    pub fn new() -> Self {
        ThinClient {
            conn: None,
            read_only: false,
        }
    }

    fn conn(&self) -> Result<&oracledb::Connection, DbError> {
        self.conn
            .as_ref()
            .ok_or_else(|| DbError::new("not connected"))
    }
}

impl Default for ThinClient {
    fn default() -> Self {
        Self::new()
    }
}

fn db_err(err: oracledb::Error) -> DbError {
    DbError::new(err.to_string())
}

/// Convert one cell, fetching the Rust type the driver documents for the
/// column's database type. Unknown/exotic types (objects, REF CURSORs, XML)
/// render as a `[TYPE]` placeholder rather than failing the whole query.
fn cell_value(
    row: &oracledb::Row,
    ix: usize,
    db_type: &oracledb::DbType,
) -> Result<Value, DbError> {
    use oracledb as odb;
    if db_type == odb::DB_TYPE_NUMBER {
        let v: Option<odb::OracleNumber> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |n| Value::Text(n.to_string())))
    } else if db_type == odb::DB_TYPE_BINARY_FLOAT {
        let v: Option<f32> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |f| Value::Float(f64::from(f))))
    } else if db_type == odb::DB_TYPE_BINARY_DOUBLE {
        let v: Option<f64> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, Value::Float))
    } else if db_type == odb::DB_TYPE_DATE
        || db_type == odb::DB_TYPE_TIMESTAMP
        || db_type == odb::DB_TYPE_TIMESTAMP_TZ
        || db_type == odb::DB_TYPE_TIMESTAMP_LTZ
    {
        let v: Option<odb::OracleTimestamp> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |t| Value::Text(t.to_string())))
    } else if db_type == odb::DB_TYPE_CHAR
        || db_type == odb::DB_TYPE_VARCHAR
        || db_type == odb::DB_TYPE_NCHAR
        || db_type == odb::DB_TYPE_NVARCHAR
        || db_type == odb::DB_TYPE_CLOB
        || db_type == odb::DB_TYPE_NCLOB
        || db_type == odb::DB_TYPE_LONG
        || db_type == odb::DB_TYPE_ROWID
        || db_type == odb::DB_TYPE_UROWID
    {
        let v: Option<String> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, Value::Text))
    } else if db_type == odb::DB_TYPE_BLOB
        || db_type == odb::DB_TYPE_RAW
        || db_type == odb::DB_TYPE_LONG_RAW
    {
        let v: Option<Vec<u8>> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, Value::Bytes))
    } else if db_type == odb::DB_TYPE_BOOLEAN {
        let v: Option<bool> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |b| {
            Value::Text(if b { "TRUE" } else { "FALSE" }.into())
        }))
    } else if db_type == odb::DB_TYPE_INTERVAL_DS {
        let v: Option<odb::OracleIntervalDS> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |t| Value::Text(t.to_string())))
    } else if db_type == odb::DB_TYPE_INTERVAL_YM {
        let v: Option<odb::OracleIntervalYM> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |t| Value::Text(t.to_string())))
    } else if db_type == odb::DB_TYPE_JSON {
        let v: Option<odb::JsonValue> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |j| Value::Text(format!("{j:?}"))))
    } else if db_type == odb::DB_TYPE_VECTOR {
        let v: Option<odb::Vector> = row.get(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |vec| Value::Text(format!("{vec:?}"))))
    } else {
        Ok(Value::Text(format!("[{db_type}]")))
    }
}

fn describe_type(
    data_type: &str,
    length: Option<oracledb::OracleNumber>,
    precision: Option<oracledb::OracleNumber>,
    scale: Option<oracledb::OracleNumber>,
) -> String {
    match data_type {
        "NUMBER" => match (precision, scale) {
            (Some(p), Some(s)) => format!("NUMBER({p},{s})"),
            (Some(p), None) => format!("NUMBER({p})"),
            _ => "NUMBER".to_string(),
        },
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => match length {
            Some(len) => format!("{data_type}({len})"),
            None => data_type.to_string(),
        },
        other => other.to_string(),
    }
}

fn fetch_rows(
    conn: &oracledb::Connection,
    sql: &str,
    binds: &[String],
    max_rows: usize,
) -> Result<QueryResult, DbError> {
    let sql = clean_statement(sql);
    let started = Instant::now();
    let params: Vec<&dyn oracledb::ToDbValue> = binds
        .iter()
        .map(|b| b as &dyn oracledb::ToDbValue)
        .collect();
    let cursor = conn.query(sql, &params).map_err(db_err)?;
    let fetching = Instant::now();
    // Column metadata comes from the cursor (not a row) so empty result
    // sets still report their columns. DbType refs are 'static, so the
    // borrow of the cursor ends here before iteration moves it.
    let columns: Vec<ColumnInfo> = cursor
        .columns()
        .iter()
        .map(|m| ColumnInfo {
            name: m.name().to_string(),
            data_type: m.data_type(),
        })
        .collect();
    let db_types: Vec<&oracledb::DbType> = cursor.columns().iter().map(|m| m.db_type()).collect();
    let mut rows = Vec::new();
    let mut truncated = false;
    for row in cursor {
        let row = row.map_err(db_err)?;
        if rows.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut values = Vec::with_capacity(db_types.len());
        for (ix, db_type) in db_types.iter().enumerate() {
            values.push(cell_value(&row, ix, db_type)?);
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

impl DbClient for ThinClient {
    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        if profile.access.read_only
            && (profile.access.sysdba || profile.username.eq_ignore_ascii_case("SYS"))
        {
            return Err(DbError::new("Read Only requires a non-SYS connection."));
        }
        // No client-side statement cache: after one failed statement a cached
        // cursor could keep failing with ORA-01008 / ORA-01002 for every later
        // query of the session until reconnect.
        let mut config = oracledb::Config::default()
            .set_credentials(&profile.username, password)
            .set_connect_string(&profile.connect_string)
            .map_err(db_err)?
            .set_stmtcachesize(0);
        if profile.access.sysdba {
            config = config.set_auth_mode(oracledb::AUTH_MODE_SYSDBA);
        }
        let conn = oracledb::connect(config).map_err(db_err)?;
        let user_row = conn
            .query_row("SELECT USER FROM DUAL", &[])
            .map_err(db_err)?;
        let user: String = user_row.get(0).map_err(db_err)?;
        let version = conn.version().map(|v| v.to_string()).unwrap_or_default();
        let database = conn.db_name().unwrap_or("").to_string();
        self.read_only = profile.access.read_only;
        self.conn = Some(conn);
        Ok(DbInfo {
            user,
            database,
            version,
        })
    }

    fn disconnect(&mut self) {
        if let Some(mut conn) = self.conn.take() {
            let _ = conn.close();
        }
    }

    fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    fn ping(&self) -> Result<(), DbError> {
        let conn = self.conn()?;
        let timeout = conn.call_timeout().map_err(db_err)?;
        conn.set_call_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(db_err)?;
        let result = conn.ping().map_err(db_err);
        let reset = conn.set_call_timeout(timeout).map_err(db_err);
        result.and(reset)
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
        let conn = self.conn()?;
        if !self.read_only {
            return fetch_rows(conn, sql, binds, max_rows);
        }
        conn.execute("SET TRANSACTION READ ONLY", &[])
            .map_err(db_err)?;
        let result = fetch_rows(conn, sql, binds, max_rows);
        let rollback = conn.rollback().map_err(db_err);
        result.and_then(|rows| rollback.map(|()| rows))
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let result = conn.execute(sql, &[]).map_err(db_err)?;
        Ok(result.rows_affected())
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let params: Vec<&dyn oracledb::ToDbValue> = binds
            .iter()
            .map(|b| b as &dyn oracledb::ToDbValue)
            .collect();
        let result = conn.execute(sql, &params).map_err(db_err)?;
        Ok(result.rows_affected())
    }

    fn commit(&mut self) -> Result<(), DbError> {
        self.conn()?.commit().map_err(db_err)
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        self.conn()?.rollback().map_err(db_err)
    }

    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let cursor = conn
            .query(
                // A materialized view is listed twice by the dictionary: as
                // MATERIALIZED VIEW and as its container TABLE. Hide the
                // container table so the MV shows up in its own category only.
                // Database links are not in ALL_OBJECTS at all; they come from
                // ALL_DB_LINKS (own links plus PUBLIC ones, so only the
                // schema's own are kept).
                "SELECT owner, object_name, object_type FROM (\
                 SELECT o.owner, o.object_name, o.object_type FROM all_objects o \
                 WHERE o.owner = :1 AND o.object_type IN \
                 ('TABLE','VIEW','MATERIALIZED VIEW','INDEX','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TRIGGER','SYNONYM','TYPE') \
                 AND NOT (o.object_type = 'TYPE' AND o.object_name LIKE 'SYS\\_PLSQL\\_%' ESCAPE '\\') \
                 AND NOT (o.object_type = 'TABLE' AND EXISTS \
                   (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name)) \
                 UNION ALL \
                 SELECT l.owner, l.db_link, 'DATABASE LINK' FROM all_db_links l WHERE l.owner = :2) \
                 ORDER BY object_type, object_name",
                &[&owner_param, &owner_param],
            )
            .map_err(db_err)?;
        let mut objects = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let owner: String = row.get(0).map_err(db_err)?;
            let name: String = row.get(1).map_err(db_err)?;
            let raw_type: String = row.get(2).map_err(db_err)?;
            objects.push(SchemaObject {
                owner,
                name,
                kind: ObjectKind::from_oracle_type(&raw_type),
            });
        }
        Ok(objects)
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let cursor = conn
            .query("SELECT username FROM all_users ORDER BY username", &[])
            .map_err(db_err)?;
        let mut schemas = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            schemas.push(name);
        }
        Ok(schemas)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        let conn = self.conn()?;
        // End each statement with `;`/`/` and pretty-print, like Toad's
        // View Script. Session-scoped, so setting it every call is cheap and
        // never leaks into other sessions.
        conn.execute(
            "BEGIN \
             DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'SQLTERMINATOR', TRUE); \
             DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'PRETTY', TRUE); \
             END;",
            &[],
        )
        .map_err(db_err)?;
        // DBMS_METADATA spells multi-word types with an underscore
        // (MATERIALIZED_VIEW).
        let type_owned = match kind {
            ObjectKind::DbLink => "DB_LINK".to_string(),
            other => other.oracle_type().replace(' ', "_"),
        };
        let type_param: &str = &type_owned;
        let name_param: &str = name;
        let owner_param: &str = owner;
        let cursor = conn
            .query(
                "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                &[&type_param, &name_param, &owner_param],
            )
            .map_err(db_err)?;
        let Some(row) = cursor.into_iter().next() else {
            return Err(DbError::new(format!("DDL kosong untuk {owner}.{name}")));
        };
        let ddl: Option<String> = row.map_err(db_err)?.get(0).map_err(db_err)?;
        let mut ddl = ddl.unwrap_or_default().trim().to_string();
        // GET_DDL('TYPE') is the spec only; add the body when there is one.
        if matches!(kind, ObjectKind::Type) {
            let body_type: &str = "TYPE_BODY";
            if let Ok(cursor) = conn.query(
                "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                &[&body_type, &name_param, &owner_param],
            ) {
                let body = cursor
                    .into_iter()
                    .next()
                    .and_then(|row| row.ok())
                    .and_then(|row| row.get::<Option<String>>(0).ok().flatten());
                if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
                    ddl = format!("{ddl}\n\n{}", body.trim());
                }
            }
        }
        Ok(ddl)
    }

    fn describe_table(&mut self, owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let name_upper = name.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let name_param: &str = &name_upper;
        let cursor = conn
            .query(
                "SELECT column_name, data_type, data_length, data_precision, data_scale, \
                 nullable, data_default FROM all_tab_columns \
                 WHERE owner = :1 AND table_name = :2 ORDER BY column_id",
                &[&owner_param, &name_param],
            )
            .map_err(db_err)?;
        let mut defs = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            let data_type: String = row.get(1).map_err(db_err)?;
            let length: Option<oracledb::OracleNumber> = row.get(2).map_err(db_err)?;
            let precision: Option<oracledb::OracleNumber> = row.get(3).map_err(db_err)?;
            let scale: Option<oracledb::OracleNumber> = row.get(4).map_err(db_err)?;
            let nullable_flag: String = row.get(5).map_err(db_err)?;
            let default: Option<String> = row.get(6).map_err(db_err)?;
            defs.push(ColumnDef {
                name,
                data_type: describe_type(&data_type, length, precision, scale),
                nullable: nullable_flag == "Y",
                default: default.map(|d| d.trim().to_string()),
            });
        }
        if defs.is_empty() {
            return Err(DbError::new(format!(
                "no such table or view: {owner_upper}.{name_upper}"
            )));
        }
        Ok(defs)
    }

    fn catalog_objects(&mut self, owner: &str) -> Result<Vec<CatalogObject>, DbError> {
        // Two dictionary queries for the whole schema instead of one
        // describe per table: a real schema can hold thousands of tables.
        let objects = self.list_objects(owner)?;
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let cursor = conn
            .query(
                "SELECT table_name, column_name, data_type, data_length, data_precision, \
                 data_scale, nullable FROM all_tab_columns \
                 WHERE owner = :1 ORDER BY table_name, column_id",
                &[&owner_param],
            )
            .map_err(db_err)?;
        let mut columns: HashMap<String, Vec<ColumnDef>> = HashMap::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let table: String = row.get(0).map_err(db_err)?;
            let name: String = row.get(1).map_err(db_err)?;
            let data_type: String = row.get(2).map_err(db_err)?;
            let length: Option<oracledb::OracleNumber> = row.get(3).map_err(db_err)?;
            let precision: Option<oracledb::OracleNumber> = row.get(4).map_err(db_err)?;
            let scale: Option<oracledb::OracleNumber> = row.get(5).map_err(db_err)?;
            let nullable_flag: String = row.get(6).map_err(db_err)?;
            columns.entry(table).or_default().push(ColumnDef {
                name,
                data_type: describe_type(&data_type, length, precision, scale),
                nullable: nullable_flag == "Y",
                default: None,
            });
        }
        Ok(objects
            .into_iter()
            .map(|object| CatalogObject {
                columns: match object.kind {
                    ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                        columns.remove(&object.name).unwrap_or_default()
                    }
                    _ => Vec::new(),
                },
                name: object.name,
                kind: object.kind,
            })
            .collect())
    }

    fn list_indexes(&mut self, owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let table_upper = table.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let table_param: &str = &table_upper;
        // One row per index column, in key order; grouped below. (No
        // LISTAGG: it raises ORA-01489 on very wide keys.)
        let cursor = conn
            .query(
                "SELECT i.index_name, i.uniqueness, i.index_type, i.status, c.column_name \
                 FROM all_indexes i \
                 LEFT JOIN all_ind_columns c \
                   ON c.index_owner = i.owner AND c.index_name = i.index_name \
                 WHERE i.table_owner = :1 AND i.table_name = :2 \
                 ORDER BY i.index_name, c.column_position",
                &[&owner_param, &table_param],
            )
            .map_err(db_err)?;
        let mut indexes: Vec<IndexDef> = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            let uniqueness: Option<String> = row.get(1).map_err(db_err)?;
            let index_type: Option<String> = row.get(2).map_err(db_err)?;
            let status: Option<String> = row.get(3).map_err(db_err)?;
            let column: Option<String> = row.get(4).map_err(db_err)?;
            if indexes.last().map(|i| i.name.as_str()) != Some(name.as_str()) {
                indexes.push(IndexDef {
                    name,
                    unique: uniqueness.as_deref() == Some("UNIQUE"),
                    index_type: index_type.unwrap_or_default(),
                    columns: Vec::new(),
                    status: status.unwrap_or_default(),
                });
            }
            if let (Some(index), Some(column)) = (indexes.last_mut(), column) {
                index.columns.push(column);
            }
        }
        Ok(indexes)
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        run_explain_plan(conn, sql)?;
        let cursor = conn
            .query(
                "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY)",
                &[],
            )
            .map_err(db_err)?;
        let mut lines = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let line: String = row.get(0).map_err(db_err)?;
            lines.push(line.trim_end().to_string());
        }
        Ok(lines)
    }

    fn explain_plan_nodes(&mut self, sql: &str) -> Result<Vec<PlanNode>, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        run_explain_plan(conn, sql)?;
        // Each EXPLAIN PLAN replaces the rows for its (null) statement id,
        // so this query sees only the plan just generated. The hierarchical
        // walk returns rows in tree order, siblings ordered by POSITION.
        let cursor = conn
            .query(
                "SELECT id, parent_id, operation, options, object_name, cost, cardinality \
                 FROM plan_table START WITH id = 0 \
                 CONNECT BY PRIOR id = parent_id ORDER SIBLINGS BY position",
                &[],
            )
            .map_err(db_err)?;
        let mut nodes = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let id: Option<oracledb::OracleNumber> = row.get(0).map_err(db_err)?;
            let parent_id: Option<oracledb::OracleNumber> = row.get(1).map_err(db_err)?;
            let operation: Option<String> = row.get(2).map_err(db_err)?;
            let options: Option<String> = row.get(3).map_err(db_err)?;
            let object_name: Option<String> = row.get(4).map_err(db_err)?;
            let cost: Option<oracledb::OracleNumber> = row.get(5).map_err(db_err)?;
            let cardinality: Option<oracledb::OracleNumber> = row.get(6).map_err(db_err)?;
            nodes.push(PlanNode {
                id: id.and_then(number_to_i64).unwrap_or(0),
                parent_id: parent_id.and_then(number_to_i64),
                operation: operation.unwrap_or_default(),
                options: options.unwrap_or_default(),
                object_name: object_name.unwrap_or_default(),
                cost: cost.and_then(number_to_i64),
                cardinality: cardinality.and_then(number_to_i64),
            });
        }
        Ok(nodes)
    }
}

/// Run `EXPLAIN PLAN FOR <sql>`, translating a missing PLAN_TABLE into an
/// error that tells the user how to create it.
fn run_explain_plan(conn: &oracledb::Connection, sql: &str) -> Result<(), DbError> {
    if let Err(err) = conn.execute(&format!("EXPLAIN PLAN FOR {sql}"), &[]) {
        let mut message = err.to_string();
        if message.contains("02404") || message.contains("PLAN_TABLE") {
            message.push_str(
                "\nHint: jalankan @?/rdbms/admin/catplan.sql sebagai user ini \
                 untuk membuat PLAN_TABLE.",
            );
        }
        return Err(DbError::new(message));
    }
    Ok(())
}

/// `PLAN_TABLE` cost/cardinality are NUMBERs; the UI only needs whole
/// values, and the optimizer reports them as integers. Non-integer or
/// unparseable values become `None` rather than failing the whole plan.
fn number_to_i64(n: oracledb::OracleNumber) -> Option<i64> {
    n.to_string().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_client_starts_disconnected() {
        let mut client = ThinClient::new();
        assert!(!client.is_connected());
        assert!(client.query("SELECT 1 FROM DUAL", 10).is_err());
        assert!(client.explain_plan_nodes("SELECT 1 FROM DUAL").is_err());
        client.disconnect(); // safe when already disconnected
    }

    #[test]
    fn connect_string_parsing_needs_no_database() {
        let easy = oracledb::Config::default()
            .set_credentials("hr", "password")
            .set_connect_string("localhost:1521/orclpdb");
        assert!(easy.is_ok());
        let descriptor = oracledb::Config::default()
            .set_connect_string("(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=dbhost)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=orcl)))");
        assert!(descriptor.is_ok());
    }

    #[test]
    fn connect_string_validation_rejects_blank_and_garbage() {
        let blank = oracledb::Config::default().set_connect_string("");
        assert!(blank.is_err(), "empty connect string should not parse");
        let garbage = oracledb::Config::default().set_connect_string("::::");
        assert!(garbage.is_err(), "garbage connect string should not parse");
    }

    #[test]
    fn thin_client_connect_reports_refused_connection() {
        let mut client = ThinClient::new();
        let err = client
            .connect(
                &ora_core::ConnectionProfile {
                    name: "refused".into(),
                    username: "hr".into(),
                    connect_string: "127.0.0.1:1/nonexistent".into(),
                    access: Default::default(),
                },
                "password",
            )
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(!client.is_connected());
    }
}
