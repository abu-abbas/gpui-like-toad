//! The explorer's catalog for SQLite: schemas are the attached databases
//! (`main` plus any `ATTACH`ed file), objects come from `sqlite_master`,
//! columns and indexes from the `pragma_*` table functions.

use ora_core::{
    ColumnDef, ConstraintDef, ConstraintKind, DbError, IndexDef, ObjectKind, SchemaObject,
};
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

/// Primary key, UNIQUE, foreign key and CHECK constraints of one table.
/// CHECKs are cut out of the stored `CREATE TABLE`, since SQLite has no
/// pragma for them.
pub(crate) fn list_constraints(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<ConstraintDef>, DbError> {
    let mut out = Vec::new();
    let blank = |name: &str, kind| ConstraintDef {
        name: name.to_string(),
        kind,
        columns: Vec::new(),
        ref_table: String::new(),
        ref_columns: Vec::new(),
        on_delete: String::new(),
        condition: String::new(),
    };

    let mut stmt = conn
        .prepare("SELECT name, pk FROM pragma_table_info(:1, :2) WHERE pk > 0 ORDER BY pk")
        .map_err(db_err)?;
    let pk = stmt
        .query_map([table, schema], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    if !pk.is_empty() {
        let mut c = blank("PRIMARY KEY", ConstraintKind::PrimaryKey);
        c.columns = pk;
        out.push(c);
    }

    for index in list_indexes(conn, schema, table)? {
        if index.index_type == "UNIQUE CONSTRAINT" {
            let mut c = blank(&index.name, ConstraintKind::Unique);
            c.columns = index.columns;
            out.push(c);
        }
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, \"table\", \"from\", \"to\", on_delete \
             FROM pragma_foreign_key_list(:1, :2) ORDER BY id, seq",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([table, schema], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    let mut last = None;
    for (id, ref_table, from, to, on_delete) in rows {
        if last != Some(id) {
            let mut c = blank(&format!("FK {id}"), ConstraintKind::ForeignKey);
            c.ref_table = ref_table;
            c.on_delete = on_delete;
            out.push(c);
            last = Some(id);
        }
        if let Some(c) = out.last_mut() {
            c.columns.push(from);
            // A missing target column means "the parent's primary key".
            c.ref_columns.push(to.unwrap_or_default());
        }
    }

    let sql = format!(
        "SELECT sql FROM {}.sqlite_master WHERE type = 'table' AND name = :1",
        quote(schema)
    );
    if let Ok(Some(ddl)) = conn.query_row(&sql, [table], |r| r.get::<_, Option<String>>(0)) {
        for condition in check_conditions(&ddl) {
            let mut c = blank("CHECK", ConstraintKind::Check);
            c.condition = condition;
            out.push(c);
        }
    }
    Ok(out)
}

/// `CREATE TRIGGER` text of the triggers on one table.
pub(crate) fn trigger_scripts(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, DbError> {
    let sql = format!(
        "SELECT sql FROM {}.sqlite_master \
         WHERE type = 'trigger' AND tbl_name = :1 AND sql IS NOT NULL ORDER BY name",
        quote(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let out = stmt
        .query_map([table], |r| r.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(out)
}

/// The text inside every `CHECK ( ... )` of a `CREATE TABLE`, skipping
/// quoted text. Used for column and table CHECKs alike.
fn check_conditions(ddl: &str) -> Vec<String> {
    let chars: Vec<char> = ddl.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            quote = Some(c);
            i += 1;
            continue;
        }
        let word_start = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        if word_start
            && chars.len() >= i + 5
            && chars[i..i + 5]
                .iter()
                .collect::<String>()
                .eq_ignore_ascii_case("check")
        {
            let mut j = i + 5;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j] == '(' {
                let (mut depth, mut q2, start) = (0i32, None::<char>, j + 1);
                let mut k = j;
                while k < chars.len() {
                    let ch = chars[k];
                    if let Some(q) = q2 {
                        if ch == q {
                            q2 = None;
                        }
                    } else if matches!(ch, '\'' | '"' | '`') {
                        q2 = Some(ch);
                    } else if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            out.push(
                                chars[start..k]
                                    .iter()
                                    .collect::<String>()
                                    .trim()
                                    .to_string(),
                            );
                            break;
                        }
                    }
                    k += 1;
                }
                i = k;
            }
        }
        i += 1;
    }
    out
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
