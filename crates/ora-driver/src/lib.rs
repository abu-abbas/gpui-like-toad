//! Oracle thin-driver implementation of [`ora_core::DbClient`].
//!
//! Uses Oracle's official `oracledb` crate in thin mode: the driver speaks
//! the Oracle Net protocol directly, so end users need no Oracle Client /
//! Instant Client install. The crate is a pre-release (`26.0.0-beta.7` at the
//! pinned revision); its API surface used here is small and covered by the
//! driver's own guide, and the `DbClient` trait isolates the GUI from it.

use std::time::Instant;

use ora_core::sql::clean_statement;
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, ObjectKind, QueryResult,
    SchemaObject, Value,
};

/// Blocking Oracle client. Owned exclusively by the GUI's background DB
/// worker thread; all methods run off the UI thread.
pub struct ThinClient {
    conn: Option<oracledb::Connection>,
}

impl ThinClient {
    pub fn new() -> Self {
        ThinClient { conn: None }
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

impl DbClient for ThinClient {
    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        let config = oracledb::Config::default()
            .set_credentials(&profile.username, password)
            .set_connect_string(&profile.connect_string)
            .map_err(db_err)?;
        let conn = oracledb::connect(config).map_err(db_err)?;
        let user_row = conn
            .query_row("SELECT USER FROM DUAL", &[])
            .map_err(db_err)?;
        let user: String = user_row.get(0).map_err(db_err)?;
        let version = conn.version().map(|v| v.to_string()).unwrap_or_default();
        let database = conn.db_name().unwrap_or("").to_string();
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

    fn query(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let started = Instant::now();
        let cursor = conn.query(sql, &[]).map_err(db_err)?;
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
        let db_types: Vec<&oracledb::DbType> =
            cursor.columns().iter().map(|m| m.db_type()).collect();
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
            elapsed: started.elapsed(),
        })
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let result = conn.execute(sql, &[]).map_err(db_err)?;
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
                "SELECT owner, object_name, object_type FROM all_objects \
                 WHERE owner = :1 AND object_type IN \
                 ('TABLE','VIEW','INDEX','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TRIGGER','SYNONYM') \
                 ORDER BY object_type, object_name",
                &[&owner_param],
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

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_client_starts_disconnected() {
        let mut client = ThinClient::new();
        assert!(!client.is_connected());
        assert!(client.query("SELECT 1 FROM DUAL", 10).is_err());
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
}
