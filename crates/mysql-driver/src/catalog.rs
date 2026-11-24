//! The explorer's catalog for MySQL and MariaDB, from
//! `information_schema` and `SHOW CREATE`. A schema is a database. Names
//! that go into SQL pass through [`lit`] or [`quote`].

use ora_core::completion::CatalogObject;
use ora_core::{
    ColumnDef, ConstraintDef, ConstraintKind, DbError, IndexDef, ObjectKind, SchemaObject,
};

use crate::conv::{lit, quote};
use crate::rowkey::{KeyColumn, RowKey};
use crate::MysqlClient;

type Row = Vec<Option<String>>;

fn col(row: &Row, i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// A list built with `GROUP_CONCAT(.. SEPARATOR '\t')`.
fn list(row: &Row, i: usize) -> Vec<String> {
    let text = col(row, i);
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\t').map(str::to_string).collect()
    }
}

const SYSTEM_SCHEMAS: &str = "'information_schema','mysql','performance_schema','sys'";

/// Databases the user can browse; the system ones are left out, as
/// Oracle's SYS objects are.
pub(crate) fn list_schemas(db: &mut MysqlClient) -> Result<Vec<String>, DbError> {
    let rows = db.text_rows(&format!(
        "SELECT schema_name FROM information_schema.schemata \
         WHERE schema_name NOT IN ({SYSTEM_SCHEMAS}) ORDER BY schema_name"
    ))?;
    Ok(rows.iter().map(|r| col(r, 0)).collect())
}

pub(crate) fn list_objects(
    db: &mut MysqlClient,
    schema: &str,
) -> Result<Vec<SchemaObject>, DbError> {
    let s = lit(schema);
    let mut out = Vec::new();
    let mut push = |name: String, kind: ObjectKind, parent: Option<String>| {
        out.push(SchemaObject {
            owner: schema.to_string(),
            name,
            kind,
            parent,
            detail: None,
        });
    };
    for r in db.text_rows(&format!(
        "SELECT table_name, table_type FROM information_schema.tables \
         WHERE table_schema = {s} AND table_type IN ('BASE TABLE','VIEW') ORDER BY table_name"
    ))? {
        let kind = if col(&r, 1) == "VIEW" {
            ObjectKind::View
        } else {
            ObjectKind::Table
        };
        push(col(&r, 0), kind, None);
    }
    for r in db.text_rows(&format!(
        "SELECT routine_name, routine_type FROM information_schema.routines \
         WHERE routine_schema = {s} ORDER BY routine_name"
    ))? {
        let kind = if col(&r, 1) == "PROCEDURE" {
            ObjectKind::Procedure
        } else {
            ObjectKind::Function
        };
        push(col(&r, 0), kind, None);
    }
    for r in db.text_rows(&format!(
        "SELECT trigger_name, event_object_table FROM information_schema.triggers \
         WHERE trigger_schema = {s} ORDER BY trigger_name"
    ))? {
        push(col(&r, 0), ObjectKind::Trigger, Some(col(&r, 1)));
    }
    for r in db.text_rows(&format!(
        "SELECT DISTINCT index_name, table_name FROM information_schema.statistics \
         WHERE table_schema = {s} AND index_name <> 'PRIMARY' ORDER BY index_name, table_name"
    ))? {
        push(col(&r, 0), ObjectKind::Index, Some(col(&r, 1)));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The default of a column as SQL that can be written back.
fn default_text(db: &MysqlClient, default: Option<String>, extra: &str) -> Option<String> {
    let extra = extra.to_ascii_lowercase();
    if extra.contains("auto_increment") {
        return Some("AUTO_INCREMENT".to_string());
    }
    let value = default?;
    if db.is_mariadb() {
        // MariaDB already writes strings quoted and a missing default as NULL.
        return (!value.eq_ignore_ascii_case("null")).then_some(value);
    }
    if extra.contains("default_generated")
        || value.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP")
    {
        return Some(value);
    }
    Some(lit(&value))
}

const COLUMN_SQL: &str = "column_name, column_type, is_nullable, column_default, extra";

pub(crate) fn describe_table(
    db: &mut MysqlClient,
    schema: &str,
    name: &str,
) -> Result<Vec<ColumnDef>, DbError> {
    let rows = db.text_rows(&format!(
        "SELECT {COLUMN_SQL} FROM information_schema.columns \
         WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
        lit(schema),
        lit(name)
    ))?;
    Ok(rows
        .iter()
        .map(|r| ColumnDef {
            name: col(r, 0),
            data_type: col(r, 1),
            nullable: col(r, 2) == "YES",
            default: default_text(db, r.get(3).cloned().flatten(), &col(r, 4)),
        })
        .collect())
}

/// Objects and the columns of tables and views in two queries.
pub(crate) fn catalog_objects(
    db: &mut MysqlClient,
    schema: &str,
) -> Result<Vec<CatalogObject>, DbError> {
    let objects = list_objects(db, schema)?;
    let rows = db.text_rows(&format!(
        "SELECT table_name, {COLUMN_SQL} FROM information_schema.columns \
         WHERE table_schema = {} ORDER BY table_name, ordinal_position",
        lit(schema)
    ))?;
    let mut by_table: std::collections::HashMap<String, Vec<ColumnDef>> = Default::default();
    for r in &rows {
        let def = ColumnDef {
            name: col(r, 1),
            data_type: col(r, 2),
            nullable: col(r, 3) == "YES",
            default: default_text(db, r.get(4).cloned().flatten(), &col(r, 5)),
        };
        by_table.entry(col(r, 0)).or_default().push(def);
    }
    Ok(objects
        .into_iter()
        .map(|o| CatalogObject {
            columns: if matches!(o.kind, ObjectKind::Table | ObjectKind::View) {
                by_table.remove(&o.name).unwrap_or_default()
            } else {
                Vec::new()
            },
            name: o.name,
            kind: o.kind,
        })
        .collect())
}

pub(crate) fn list_indexes(
    db: &mut MysqlClient,
    schema: &str,
    table: &str,
) -> Result<Vec<IndexDef>, DbError> {
    let rows = db.text_rows(&format!(
        "SELECT index_name, MIN(non_unique), MIN(index_type), \
                GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR '\\t') \
         FROM information_schema.statistics \
         WHERE table_schema = {} AND table_name = {} \
         GROUP BY index_name ORDER BY index_name = 'PRIMARY' DESC, index_name",
        lit(schema),
        lit(table)
    ))?;
    Ok(rows
        .iter()
        .map(|r| IndexDef {
            name: col(r, 0),
            unique: col(r, 1) == "0",
            index_type: col(r, 2),
            columns: list(r, 3),
            status: "VALID".to_string(),
        })
        .collect())
}

pub(crate) fn list_constraints(
    db: &mut MysqlClient,
    schema: &str,
    table: &str,
) -> Result<Vec<ConstraintDef>, DbError> {
    let (s, t) = (lit(schema), lit(table));
    let rows = db.text_rows(&format!(
        "SELECT tc.constraint_name, tc.constraint_type, \
                GROUP_CONCAT(k.column_name ORDER BY k.ordinal_position SEPARATOR '\\t'), \
                MAX(k.referenced_table_schema), MAX(k.referenced_table_name), \
                GROUP_CONCAT(k.referenced_column_name ORDER BY k.ordinal_position SEPARATOR '\\t'), \
                MAX(rc.delete_rule) \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage k \
           ON k.constraint_schema = tc.constraint_schema AND k.table_name = tc.table_name \
          AND k.constraint_name = tc.constraint_name \
         LEFT JOIN information_schema.referential_constraints rc \
           ON rc.constraint_schema = tc.constraint_schema AND rc.table_name = tc.table_name \
          AND rc.constraint_name = tc.constraint_name \
         WHERE tc.table_schema = {s} AND tc.table_name = {t} \
           AND tc.constraint_type IN ('PRIMARY KEY','UNIQUE','FOREIGN KEY') \
         GROUP BY tc.constraint_name, tc.constraint_type \
         ORDER BY tc.constraint_type, tc.constraint_name"
    ))?;
    let mut out: Vec<ConstraintDef> = rows
        .iter()
        .map(|r| {
            let kind = match col(r, 1).as_str() {
                "PRIMARY KEY" => ConstraintKind::PrimaryKey,
                "UNIQUE" => ConstraintKind::Unique,
                _ => ConstraintKind::ForeignKey,
            };
            ConstraintDef {
                name: col(r, 0),
                kind,
                columns: list(r, 2),
                // Only a parent in the same database can be copied by name.
                ref_table: if kind == ConstraintKind::ForeignKey && col(r, 3) == schema {
                    col(r, 4)
                } else {
                    String::new()
                },
                ref_columns: list(r, 5),
                on_delete: match col(r, 6).as_str() {
                    "CASCADE" => "CASCADE",
                    "SET NULL" => "SET NULL",
                    _ => "NO ACTION",
                }
                .to_string(),
                condition: String::new(),
            }
        })
        .collect();
    // CHECK constraints exist from MySQL 8.0.16 and MariaDB 10.2; older
    // servers have no table for them.
    if let Ok(checks) = db.text_rows(&format!(
        "SELECT tc.constraint_name, cc.check_clause \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.check_constraints cc \
           ON cc.constraint_schema = tc.constraint_schema \
          AND cc.constraint_name = tc.constraint_name \
         WHERE tc.table_schema = {s} AND tc.table_name = {t} AND tc.constraint_type = 'CHECK' \
         ORDER BY tc.constraint_name"
    )) {
        out.extend(checks.iter().map(|r| ConstraintDef {
            name: col(r, 0),
            kind: ConstraintKind::Check,
            columns: Vec::new(),
            ref_table: String::new(),
            ref_columns: Vec::new(),
            on_delete: "NO ACTION".to_string(),
            condition: col(r, 1),
        }));
    }
    Ok(out)
}

pub(crate) fn trigger_scripts(
    db: &mut MysqlClient,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, DbError> {
    let names = db.text_rows(&format!(
        "SELECT trigger_name FROM information_schema.triggers \
         WHERE trigger_schema = {} AND event_object_table = {} ORDER BY trigger_name",
        lit(schema),
        lit(table)
    ))?;
    let mut out = Vec::new();
    for r in names {
        out.extend(show_create(db, "TRIGGER", schema, &col(&r, 0), 2)?);
    }
    Ok(out)
}

/// The text column `at` of `SHOW CREATE <what> schema.name`.
fn show_create(
    db: &mut MysqlClient,
    what: &str,
    schema: &str,
    name: &str,
    at: usize,
) -> Result<Option<String>, DbError> {
    let rows = db.text_rows(&format!(
        "SHOW CREATE {what} {}.{}",
        quote(schema),
        quote(name)
    ))?;
    Ok(rows.first().and_then(|r| r.get(at).cloned().flatten()))
}

/// The script of one object.
pub(crate) fn object_ddl(
    db: &mut MysqlClient,
    schema: &str,
    name: &str,
    kind: &ObjectKind,
) -> Result<String, DbError> {
    let script = match kind {
        ObjectKind::Table => show_create(db, "TABLE", schema, name, 1)?,
        ObjectKind::View => show_create(db, "VIEW", schema, name, 1)?,
        ObjectKind::Function => show_create(db, "FUNCTION", schema, name, 2)?,
        ObjectKind::Procedure => show_create(db, "PROCEDURE", schema, name, 2)?,
        ObjectKind::Trigger => show_create(db, "TRIGGER", schema, name, 2)?,
        ObjectKind::Index => index_script(db, schema, name)?,
        other => {
            return Err(DbError::new(format!(
                "MySQL: no script for {}",
                other.group_label()
            )))
        }
    };
    match script {
        Some(text) if !text.trim().is_empty() => {
            Ok(format!("{};", text.trim_end().trim_end_matches(';')))
        }
        _ => Err(DbError::new(format!(
            "MySQL: {name} was not found, or its definition is hidden from this user"
        ))),
    }
}

fn index_script(db: &mut MysqlClient, schema: &str, name: &str) -> Result<Option<String>, DbError> {
    let rows = db.text_rows(&format!(
        "SELECT table_name, MIN(non_unique), MIN(index_type), \
                GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR '\\t') \
         FROM information_schema.statistics \
         WHERE table_schema = {} AND index_name = {} GROUP BY table_name LIMIT 1",
        lit(schema),
        lit(name)
    ))?;
    Ok(rows.first().map(|r| {
        let cols: Vec<String> = list(r, 3).iter().map(|c| quote(c)).collect();
        let kind = match col(r, 2).as_str() {
            "FULLTEXT" => "FULLTEXT ",
            "SPATIAL" => "SPATIAL ",
            _ if col(r, 1) == "0" => "UNIQUE ",
            _ => "",
        };
        format!(
            "CREATE {kind}INDEX {} ON {}.{} ({})",
            quote(name),
            quote(schema),
            quote(&col(r, 0)),
            cols.join(", ")
        )
    }))
}

/// The key of a table: its primary key, or else every column that can be
/// compared exactly (not LOBs, floating point or bit fields).
pub(crate) fn primary_key(
    db: &mut MysqlClient,
    schema: &str,
    table: &str,
) -> Result<RowKey, DbError> {
    let rows = db.text_rows(&format!(
        "SELECT c.column_name, c.data_type \
         FROM information_schema.key_column_usage k \
         JOIN information_schema.columns c \
           ON c.table_schema = k.table_schema AND c.table_name = k.table_name \
          AND c.column_name = k.column_name \
         WHERE k.table_schema = {} AND k.table_name = {} AND k.constraint_name = 'PRIMARY' \
         ORDER BY k.ordinal_position",
        lit(schema),
        lit(table)
    ))?;
    if !rows.is_empty() {
        return Ok(RowKey {
            columns: rows
                .iter()
                .map(|r| KeyColumn::new(&col(r, 0), &col(r, 1)))
                .collect(),
            all_columns: false,
        });
    }
    let rows = db.text_rows(&format!(
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_schema = {} AND table_name = {} \
           AND data_type NOT IN ('tinyblob','blob','mediumblob','longblob','tinytext','text', \
               'mediumtext','longtext','json','geometry','point','linestring','polygon', \
               'multipoint','multilinestring','multipolygon','geometrycollection', \
               'float','double','bit') \
           AND (extra NOT LIKE '%GENERATED%' OR extra LIKE '%DEFAULT_GENERATED%') \
         ORDER BY ordinal_position",
        lit(schema),
        lit(table)
    ))?;
    Ok(RowKey {
        columns: rows
            .iter()
            .map(|r| KeyColumn::new(&col(r, 0), &col(r, 1)))
            .collect(),
        all_columns: true,
    })
}
