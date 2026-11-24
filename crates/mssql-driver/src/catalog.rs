//! SQL Server's catalog: databases, objects, columns, keys and scripts.
//! Every query names its database (`[db].sys.objects`), so none of it
//! depends on which database the session is in.

use ora_core::completion::CatalogObject;
use ora_core::{ColumnDef, DbError, IndexDef, ObjectKind, SchemaObject};

use crate::conv::{lit, quote};
use crate::MssqlClient;

pub(crate) fn list_databases(c: &mut MssqlClient) -> Result<Vec<String>, DbError> {
    Ok(c.text_rows(
        "SELECT name FROM sys.databases \
         WHERE state = 0 AND HAS_DBACCESS(name) = 1 ORDER BY name",
    )?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .collect())
}

fn kind_of(type_code: &str) -> Option<ObjectKind> {
    Some(match type_code.trim() {
        "U" => ObjectKind::Table,
        "V" => ObjectKind::View,
        "P" | "PC" => ObjectKind::Procedure,
        "FN" | "IF" | "TF" | "FS" | "FT" => ObjectKind::Function,
        "TR" | "TA" => ObjectKind::Trigger,
        "SO" => ObjectKind::Sequence,
        "SN" => ObjectKind::Synonym,
        _ => return None,
    })
}

/// Objects of every schema of `database` (an empty name is the session's
/// current database).
pub(crate) fn list_objects(
    c: &mut MssqlClient,
    database: &str,
) -> Result<Vec<SchemaObject>, DbError> {
    let db = c.database_of(Some(database))?;
    let db = quote(&db);
    let sql = format!(
        "SELECT s.name, o.name, o.type, p.name \
         FROM {db}.sys.objects o \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         LEFT JOIN {db}.sys.objects p ON p.object_id = o.parent_object_id AND o.type IN ('TR', 'TA') \
         WHERE o.is_ms_shipped = 0 AND o.type IN ('U','V','P','PC','FN','IF','TF','FS','FT','TR','TA','SO','SN') \
         ORDER BY s.name, o.name"
    );
    let mut out = Vec::new();
    for row in c.text_rows(&sql)? {
        let [Some(owner), Some(name), Some(code), parent] = &row[..] else {
            continue;
        };
        let Some(kind) = kind_of(code) else { continue };
        out.push(SchemaObject {
            owner: owner.clone(),
            name: name.clone(),
            kind,
            parent: parent.clone(),
            detail: None,
        });
    }
    Ok(out)
}

/// `type(length)` as it reads in a column definition.
fn type_text(name: &str, max_length: i64, precision: i64, scale: i64) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "char" | "varchar" | "binary" | "varbinary" => {
            if max_length < 0 {
                format!("{lower}(MAX)")
            } else {
                format!("{lower}({max_length})")
            }
        }
        "nchar" | "nvarchar" => {
            if max_length < 0 {
                format!("{lower}(MAX)")
            } else {
                format!("{lower}({})", max_length / 2)
            }
        }
        "decimal" | "numeric" => format!("{lower}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" => format!("{lower}({scale})"),
        _ => lower,
    }
}

pub(crate) fn describe_table(
    c: &mut MssqlClient,
    database: &str,
    owner: &str,
    table: &str,
) -> Result<Vec<ColumnDef>, DbError> {
    let db = quote(database);
    let sql = format!(
        "SELECT c.name, t.name, c.max_length, c.precision, c.scale, c.is_nullable, \
                dc.definition, c.is_identity \
         FROM {db}.sys.columns c \
         JOIN {db}.sys.objects o ON o.object_id = c.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         JOIN {db}.sys.types t ON t.user_type_id = c.user_type_id \
         LEFT JOIN {db}.sys.default_constraints dc ON dc.object_id = c.default_object_id \
         WHERE s.name = {} AND o.name = {} \
         ORDER BY c.column_id",
        lit(owner),
        lit(table)
    );
    let rows = c.text_rows(&sql)?;
    if rows.is_empty() {
        return Err(DbError::new(format!(
            "{database}.{owner}.{table} has no columns, or it does not exist."
        )));
    }
    let num = |v: &Option<String>| {
        v.as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    };
    Ok(rows
        .into_iter()
        .map(|r| ColumnDef {
            name: r[0].clone().unwrap_or_default(),
            data_type: type_text(
                r[1].as_deref().unwrap_or(""),
                num(&r[2]),
                num(&r[3]),
                num(&r[4]),
            ),
            nullable: r[5].as_deref() == Some("1"),
            default: if r[7].as_deref() == Some("1") {
                Some("IDENTITY".to_string())
            } else {
                r[6].clone()
            },
        })
        .collect())
}

/// The key columns a table is paged by: the primary key, else the first
/// unique index; empty when it has neither (a heap, or a view).
pub(crate) fn order_key(
    c: &mut MssqlClient,
    database: &str,
    owner: &str,
    table: &str,
) -> Result<Vec<String>, DbError> {
    let db = quote(database);
    let sql = format!(
        "SELECT i.index_id, c.name \
         FROM {db}.sys.indexes i \
         JOIN {db}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {db}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         JOIN {db}.sys.objects o ON o.object_id = i.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = {} AND o.name = {} AND i.is_disabled = 0 AND i.has_filter = 0 \
           AND (i.is_primary_key = 1 OR i.is_unique = 1) AND ic.is_included_column = 0 \
         ORDER BY i.is_primary_key DESC, i.index_id, ic.key_ordinal",
        lit(owner),
        lit(table)
    );
    let rows = c.text_rows(&sql)?;
    // Rows come ordered by index; keep the first index's columns.
    let first = rows.first().and_then(|r| r[0].clone());
    Ok(rows
        .into_iter()
        .filter(|r| r[0] == first)
        .filter_map(|r| r[1].clone())
        .collect())
}

pub(crate) fn list_indexes(
    c: &mut MssqlClient,
    database: &str,
    owner: &str,
    table: &str,
) -> Result<Vec<IndexDef>, DbError> {
    let db = quote(database);
    let sql = format!(
        "SELECT i.name, i.is_unique, i.type_desc, i.is_disabled, c.name \
         FROM {db}.sys.indexes i \
         JOIN {db}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {db}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         JOIN {db}.sys.objects o ON o.object_id = i.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = {} AND o.name = {} AND i.type > 0 AND ic.is_included_column = 0 \
         ORDER BY i.name, ic.key_ordinal",
        lit(owner),
        lit(table)
    );
    let mut out: Vec<IndexDef> = Vec::new();
    for r in c.text_rows(&sql)? {
        let name = r[0].clone().unwrap_or_default();
        let column = r[4].clone().unwrap_or_default();
        match out.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => out.push(IndexDef {
                name,
                unique: r[1].as_deref() == Some("1"),
                index_type: r[2].clone().unwrap_or_default(),
                columns: vec![column],
                status: if r[3].as_deref() == Some("1") {
                    "DISABLED".to_string()
                } else {
                    "ENABLED".to_string()
                },
            }),
        }
    }
    Ok(out)
}

/// The script of an object: the stored text of a view, procedure,
/// function or trigger; a rebuilt `CREATE TABLE` for a table.
pub(crate) fn object_ddl(
    c: &mut MssqlClient,
    database: &str,
    owner: &str,
    name: &str,
    kind: &ObjectKind,
) -> Result<String, DbError> {
    if *kind == ObjectKind::Table {
        return table_ddl(c, database, owner, name);
    }
    let db = quote(database);
    let sql = format!(
        "SELECT m.definition FROM {db}.sys.sql_modules m \
         JOIN {db}.sys.objects o ON o.object_id = m.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = {} AND o.name = {}",
        lit(owner),
        lit(name)
    );
    c.text_rows(&sql)?
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .ok_or_else(|| DbError::new("SQL Server keeps no script for this object."))
}

fn table_ddl(
    c: &mut MssqlClient,
    database: &str,
    owner: &str,
    name: &str,
) -> Result<String, DbError> {
    let columns = describe_table(c, database, owner, name)?;
    let key = order_key(c, database, owner, name)?;
    let mut lines: Vec<String> = columns
        .iter()
        .map(|col| {
            let mut line = format!("    {} {}", quote(&col.name), col.data_type);
            if col.default.as_deref() == Some("IDENTITY") {
                line.push_str(" IDENTITY(1,1)");
            }
            line.push_str(if col.nullable { " NULL" } else { " NOT NULL" });
            if let Some(default) = col.default.as_deref().filter(|d| *d != "IDENTITY") {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();
    if !key.is_empty() {
        let key: Vec<String> = key.iter().map(|k| quote(k)).collect();
        lines.push(format!("    PRIMARY KEY ({})", key.join(", ")));
    }
    Ok(format!(
        "CREATE TABLE {}.{}.{} (\n{}\n);",
        quote(database),
        quote(owner),
        quote(name),
        lines.join(",\n")
    ))
}

/// Tables and views of `database` with their columns, for autocomplete.
pub(crate) fn catalog_objects(
    c: &mut MssqlClient,
    database: &str,
) -> Result<Vec<CatalogObject>, DbError> {
    let db = c.database_of(Some(database))?;
    let db = quote(&db);
    let sql = format!(
        "SELECT o.name, o.type, c.name, t.name, c.max_length, c.precision, c.scale, c.is_nullable \
         FROM {db}.sys.objects o \
         JOIN {db}.sys.columns c ON c.object_id = o.object_id \
         JOIN {db}.sys.types t ON t.user_type_id = c.user_type_id \
         WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V') \
         ORDER BY o.name, c.column_id"
    );
    let num = |v: &Option<String>| {
        v.as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let mut out: Vec<CatalogObject> = Vec::new();
    for r in c.text_rows(&sql)? {
        let name = r[0].clone().unwrap_or_default();
        let kind = if r[1].as_deref().is_some_and(|t| t.trim() == "V") {
            ObjectKind::View
        } else {
            ObjectKind::Table
        };
        let column = ColumnDef {
            name: r[2].clone().unwrap_or_default(),
            data_type: type_text(
                r[3].as_deref().unwrap_or(""),
                num(&r[4]),
                num(&r[5]),
                num(&r[6]),
            ),
            nullable: r[7].as_deref() == Some("1"),
            default: None,
        };
        match out.last_mut() {
            Some(last) if last.name == name && last.kind == kind => last.columns.push(column),
            _ => out.push(CatalogObject {
                name,
                kind,
                columns: vec![column],
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_types_read_like_a_definition() {
        assert_eq!(type_text("nvarchar", 100, 0, 0), "nvarchar(50)");
        assert_eq!(type_text("varchar", -1, 0, 0), "varchar(MAX)");
        assert_eq!(type_text("decimal", 9, 12, 3), "decimal(12,3)");
        assert_eq!(type_text("datetime2", 8, 27, 7), "datetime2(7)");
        assert_eq!(type_text("int", 4, 10, 0), "int");
    }
}
