//! The explorer's catalog for SQLite: schemas are the attached databases
//! (`main` plus any `ATTACH`ed file), objects come from `sqlite_master`,
//! columns and indexes from the `pragma_*` table functions.

use ora_core::{ColumnDef, DbError, IndexDef, ObjectKind, SchemaObject};
use rusqlite::Connection;

use crate::db_err;

/// Quote one identifier for SQLite, doubling embedded quotes.
fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `main` and attached databases, `main` first. `temp` is left out: it only
/// holds this session's temporary objects.
pub(crate) fn list_schemas(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq")
        .map_err(db_err)?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(names)
}

/// Tables, views, indexes and triggers of one database. SQLite's own
/// `sqlite_*` tables are left out, as Oracle's dictionary is.
pub(crate) fn list_objects(conn: &Connection, schema: &str) -> Result<Vec<SchemaObject>, DbError> {
    let sql = format!(
        "SELECT type, name FROM {}.sqlite_master \
         WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        quote(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, name) = row.map_err(db_err)?;
        out.push(SchemaObject {
            owner: schema.to_string(),
            name,
            kind: object_kind(&kind),
            parent: None,
            detail: None,
        });
    }
    Ok(out)
}

fn object_kind(sqlite_type: &str) -> ObjectKind {
    match sqlite_type {
        "table" => ObjectKind::Table,
        "view" => ObjectKind::View,
        "index" => ObjectKind::Index,
        "trigger" => ObjectKind::Trigger,
        other => ObjectKind::Other(other.to_ascii_uppercase()),
    }
}

/// The `CREATE` statement SQLite stored for the object. For a table, its
/// explicit indexes and triggers follow, like Toad's View Script.
pub(crate) fn object_ddl(conn: &Connection, schema: &str, name: &str) -> Result<String, DbError> {
    let sql = format!(
        "SELECT sql FROM {}.sqlite_master \
         WHERE sql IS NOT NULL AND (name = :1 OR (tbl_name = :1 AND type IN ('index', 'trigger'))) \
         ORDER BY CASE WHEN name = :1 THEN 0 WHEN type = 'index' THEN 1 ELSE 2 END, name",
        quote(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let parts = stmt
        .query_map([name], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    if parts.is_empty() {
        return Err(DbError::new(format!("SQLite: {name} was not found")));
    }
    Ok(parts
        .iter()
        .map(|p| format!("{};", p.trim_end().trim_end_matches(';')))
        .collect::<Vec<_>>()
        .join("\n\n"))
}

pub(crate) fn describe_table(
    conn: &Connection,
    schema: &str,
    name: &str,
) -> Result<Vec<ColumnDef>, DbError> {
    let mut stmt = conn
        .prepare("SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info(:1, :2) ORDER BY cid")
        .map_err(db_err)?;
    let cols = stmt
        .query_map([name, schema], |r| {
            Ok(ColumnDef {
                name: r.get(0)?,
                data_type: r.get::<_, String>(1)?.to_ascii_uppercase(),
                nullable: r.get::<_, i64>(2)? == 0,
                default: r.get(3)?,
            })
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(cols)
}

pub(crate) fn list_indexes(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<IndexDef>, DbError> {
    let mut stmt = conn
        .prepare("SELECT name, \"unique\", origin FROM pragma_index_list(:1, :2) ORDER BY name")
        .map_err(db_err)?;
    let indexes = stmt
        .query_map([table, schema], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    let mut columns = conn
        .prepare("SELECT name FROM pragma_index_info(:1, :2) ORDER BY seqno")
        .map_err(db_err)?;
    let mut out = Vec::with_capacity(indexes.len());
    for (name, unique, origin) in indexes {
        let cols = columns
            .query_map([name.as_str(), schema], |r| {
                Ok(r.get::<_, Option<String>>(0)?
                    .unwrap_or_else(|| "<expr>".into()))
            })
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        out.push(IndexDef {
            name,
            unique,
            // "c" CREATE INDEX, "u" UNIQUE constraint, "pk" PRIMARY KEY.
            index_type: match origin.as_str() {
                "pk" => "PRIMARY KEY",
                "u" => "UNIQUE CONSTRAINT",
                _ => "NORMAL",
            }
            .to_string(),
            columns: cols,
            status: "VALID".to_string(),
        });
    }
    Ok(out)
}

/// True for a table declared `WITHOUT ROWID`, read from its stored
/// `CREATE TABLE`.
pub(crate) fn without_rowid(conn: &Connection, schema: &str, table: &str) -> Result<bool, DbError> {
    let sql = format!(
        "SELECT sql FROM {}.sqlite_master WHERE type = 'table' AND name = :1",
        quote(schema)
    );
    let ddl: Option<String> = match conn.query_row(&sql, [table], |r| r.get(0)) {
        Ok(ddl) => ddl,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
        Err(e) => return Err(db_err(e)),
    };
    Ok(ddl.is_some_and(|ddl| declares_without_rowid(&ddl)))
}

/// `... ) WITHOUT ROWID` at the end of a `CREATE TABLE` (any case and
/// spacing, possibly followed by `, STRICT`).
fn declares_without_rowid(ddl: &str) -> bool {
    let tail = ddl.rsplit(')').next().unwrap_or("");
    let words: Vec<String> = tail
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    words
        .windows(2)
        .any(|w| w[0] == "WITHOUT" && w[1] == "ROWID")
}

#[cfg(test)]
mod tests {
    use super::declares_without_rowid;

    #[test]
    fn without_rowid_is_read_from_the_table_options() {
        assert!(declares_without_rowid(
            "CREATE TABLE t (a PRIMARY KEY) WITHOUT ROWID"
        ));
        assert!(declares_without_rowid(
            "create table t(a primary key)  without\n rowid, strict"
        ));
        assert!(!declares_without_rowid(
            "CREATE TABLE t (note TEXT DEFAULT 'without rowid')"
        ));
        assert!(!declares_without_rowid("CREATE TABLE t (a)"));
    }
}
