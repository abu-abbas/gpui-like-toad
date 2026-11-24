//! The statements of a transfer. Identifiers are quoted by the dialect;
//! values are rendered as typed literals for the target database. Nothing
//! the user typed reaches SQL unquoted except recordset conditions, which
//! [`super::spec::check_condition`] limits to one expression.

use crate::ddl::check_data_type;
use crate::dialect::{DbKind, Dialect};
use crate::model::{ColumnDef, ConstraintDef, ConstraintKind, IndexDef, Value};

use super::spec::{Fields, NameCase, TransferOptions};

/// Name of the extra first column holding the row key (keyset paging).
pub const KEY_COLUMN: &str = "__KEY__";

/// One transferred column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnPlan {
    pub source: String,
    pub target: String,
    /// Type used for the target column (and to render its literals).
    pub target_type: String,
    pub nullable: bool,
    /// DEFAULT expression to create the column with, when it is safe to.
    pub default: Option<String>,
}

pub fn convert_name(name: &str, case: Option<NameCase>) -> String {
    case.map_or_else(|| name.to_string(), |c| c.apply(name))
}

/// Column type for the target engine. The same engine keeps the type text;
/// different engines go through the neutral
/// [`crate::dialect::CanonType`].
pub fn map_type(source: DbKind, target: DbKind, data_type: &str) -> String {
    if source == target {
        return data_type.trim().to_string();
    }
    target
        .dialect()
        .native_type(&source.dialect().canonical_type(data_type))
}

/// Resolve the fields to copy and their target names and types.
pub fn plan_columns(
    source: DbKind,
    target: DbKind,
    source_columns: &[ColumnDef],
    fields: &Fields,
    case: Option<NameCase>,
) -> Result<Vec<ColumnPlan>, String> {
    let make = |col: &ColumnDef, target_name: &str| ColumnPlan {
        source: col.name.clone(),
        target: convert_name(target_name, case),
        target_type: map_type(source, target, &col.data_type),
        nullable: col.nullable,
        default: default_for(source, target, col),
    };
    match fields {
        Fields::All => Ok(source_columns.iter().map(|c| make(c, &c.name)).collect()),
        Fields::Custom(map) => map
            .iter()
            .map(|m| {
                let col = source_columns
                    .iter()
                    .find(|c| c.name == m.source)
                    .or_else(|| {
                        source_columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(&m.source))
                    })
                    .ok_or_else(|| format!("The source has no column {}.", m.source))?;
                Ok(make(col, &m.target))
            })
            .collect(),
    }
}

/// A column default that can move to the target: same engine only (the
/// expressions are engine specific), one plain expression, no sequence or
/// schema-qualified reference.
fn default_for(source: DbKind, target: DbKind, col: &ColumnDef) -> Option<String> {
    if source != target {
        return None;
    }
    let text = col.default.as_deref()?.trim();
    let upper = text.to_ascii_uppercase();
    if text.is_empty()
        || text.len() > 200
        || text.contains('\n')
        || upper == "NULL"
        || upper.contains("NEXTVAL")
        || upper.contains("CURRVAL")
        || upper.contains("IDENTITY")
        || upper == "AUTO_INCREMENT"
        || text.contains("\".\"")
        || super::spec::check_condition(text).is_err()
    {
        return None;
    }
    Some(text.to_string())
}

pub fn create_table_sql(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    columns: &[ColumnPlan],
) -> Result<String, String> {
    create_table_sql_with(dialect, schema, table, columns, &[])
}

/// `CREATE TABLE` with extra table-level clauses (constraints) after the
/// columns.
pub fn create_table_sql_with(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    columns: &[ColumnPlan],
    clauses: &[String],
) -> Result<String, String> {
    if columns.is_empty() {
        return Err("The table has no columns.".to_string());
    }
    let mut lines = Vec::new();
    for c in columns {
        let ty = check_data_type(&c.target_type)?;
        let default = c
            .default
            .as_deref()
            .map(|d| dialect.default_clause(d))
            .unwrap_or_default();
        lines.push(format!(
            "{} {ty}{default}{}",
            dialect.quote_ident(&c.target),
            if c.nullable { "" } else { " NOT NULL" }
        ));
    }
    lines.extend(clauses.iter().cloned());
    Ok(format!(
        "CREATE TABLE {} (\n  {}\n)",
        dialect.table_ref(schema, table),
        lines.join(",\n  ")
    ))
}

/// What the constraints of one table become on the target.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConstraintPlan {
    /// Clauses inside `CREATE TABLE`.
    pub inline: Vec<String>,
    /// `ALTER TABLE ... ADD` statements to run once every table is in.
    pub after: Vec<String>,
    /// One line per constraint that could not be copied, and why.
    pub skipped: Vec<String>,
    /// Column lists of the primary key / unique constraints that are copied
    /// (their backing index is then not copied again).
    pub key_columns: Vec<Vec<String>>,
}

/// Decide which constraints of a source table are created on the target.
/// Constraints are created unnamed (names are unique per schema in Oracle
/// and would collide). `resolve` maps a referenced source table to its
/// target name when that table is part of the transfer with all its
/// columns under their own names.
#[allow(clippy::too_many_arguments)]
pub fn plan_constraints(
    source: DbKind,
    dialect: &dyn Dialect,
    schema: &str,
    constraints: &[ConstraintDef],
    plan: &[ColumnPlan],
    all_fields: bool,
    options: &TransferOptions,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> ConstraintPlan {
    let target = dialect.kind();
    let mut out = ConstraintPlan::default();
    let cols = |names: &[String]| -> Option<Vec<String>> {
        names
            .iter()
            .map(|c| {
                plan.iter()
                    .find(|p| p.source == *c)
                    .map(|p| dialect.quote_ident(&p.target))
            })
            .collect()
    };
    let renamed = plan
        .iter()
        .any(|p| !p.target.eq_ignore_ascii_case(&p.source));
    for c in constraints {
        let label = match c.kind {
            ConstraintKind::PrimaryKey => "primary key".to_string(),
            ConstraintKind::Unique => format!("unique constraint {}", c.name),
            ConstraintKind::ForeignKey => format!("foreign key on ({})", c.columns.join(", ")),
            ConstraintKind::Check => "check constraint".to_string(),
        };
        let enabled = match c.kind {
            ConstraintKind::PrimaryKey => options.include_primary_keys,
            ConstraintKind::Unique => options.include_uniques,
            ConstraintKind::ForeignKey => options.include_foreign_keys,
            ConstraintKind::Check => options.include_checks,
        };
        if !enabled {
            continue;
        }
        let mut skip = |why: &str| out.skipped.push(format!("{label} not copied: {why}."));
        match c.kind {
            ConstraintKind::PrimaryKey | ConstraintKind::Unique => {
                let Some(list) = cols(&c.columns) else {
                    skip("one of its columns is not transferred");
                    continue;
                };
                let word = if c.kind == ConstraintKind::PrimaryKey {
                    "PRIMARY KEY"
                } else {
                    "UNIQUE"
                };
                out.inline.push(format!("{word} ({})", list.join(", ")));
                out.key_columns.push(c.columns.clone());
            }
            ConstraintKind::Check => {
                if source != target {
                    skip("check expressions are engine specific");
                } else if renamed || (c.columns.is_empty() && !all_fields) {
                    skip("columns were renamed or left out");
                } else if cols(&c.columns).is_none() {
                    skip("one of its columns is not transferred");
                } else if super::spec::check_condition(&c.condition).is_err()
                    || c.condition.is_empty()
                {
                    skip("the condition is not a single plain expression");
                } else {
                    out.inline.push(format!("CHECK ({})", c.condition));
                }
            }
            ConstraintKind::ForeignKey => {
                let Some(list) = cols(&c.columns) else {
                    skip("one of its columns is not transferred");
                    continue;
                };
                let Some(parent) = resolve(&c.ref_table) else {
                    skip("the referenced table is not part of this transfer");
                    continue;
                };
                let parent_ref = dialect.foreign_key_parent(schema, &parent);
                let ref_cols: Vec<String> = c
                    .ref_columns
                    .iter()
                    .filter(|r| !r.is_empty())
                    .map(|r| dialect.quote_ident(&convert_name(r, options.convert_case)))
                    .collect();
                let ref_part = if ref_cols.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", ref_cols.join(", "))
                };
                let rule = match c.on_delete.to_ascii_uppercase().as_str() {
                    "CASCADE" => " ON DELETE CASCADE",
                    "SET NULL" => " ON DELETE SET NULL",
                    _ => "",
                };
                let clause = format!(
                    "FOREIGN KEY ({}) REFERENCES {parent_ref}{ref_part}{rule}",
                    list.join(", ")
                );
                if dialect.foreign_keys_inline() {
                    out.inline.push(clause);
                } else {
                    out.after.push(clause);
                }
            }
        }
    }
    out
}

/// `CREATE INDEX` for one source index, or `None` when it cannot be
/// copied: function-based, SQLite's automatic indexes, or one that uses a
/// column that is not transferred.
pub fn create_index_sql(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    index: &IndexDef,
    columns: &[ColumnPlan],
    case: Option<NameCase>,
) -> Option<String> {
    if index.index_type.to_uppercase().contains("FUNCTION")
        || index.name.starts_with("sqlite_autoindex")
        || index.columns.is_empty()
    {
        return None;
    }
    let cols: Option<Vec<String>> = index
        .columns
        .iter()
        .map(|c| {
            columns
                .iter()
                .find(|p| p.source == *c)
                .map(|p| dialect.quote_ident(&p.target))
        })
        .collect();
    let name = convert_name(&index.name, case);
    let unique = if index.unique { "UNIQUE " } else { "" };
    let (index_ref, table_ref) = dialect.index_refs(schema, &name, table);
    Some(format!(
        "CREATE {unique}INDEX {index_ref} ON {table_ref} ({})",
        cols?.join(", ")
    ))
}

/// Where the next page of rows starts: after the key of the last row.
#[derive(Debug, Clone, PartialEq)]
pub enum Resume {
    Start,
    After(Value),
}

/// One page of the source, ordered by row key. The key comes first
/// ([`KEY_COLUMN`]) and is removed before the rows are inserted.
pub fn keyset_select_sql(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    columns: &[ColumnPlan],
    condition: Option<&str>,
    resume: &Resume,
    limit: usize,
) -> Result<String, String> {
    let key = dialect.row_id_expr();
    let sort_key = dialect.row_id_sort_expr();
    let mut conds = Vec::new();
    if let Some(c) = condition {
        conds.push(format!("({c})"));
    }
    if let Resume::After(value) = resume {
        conds.push(dialect.keyset_after(sort_key, value)?);
    }
    let list: Vec<String> = columns
        .iter()
        .map(|c| format!("t.{}", dialect.quote_ident(&c.source)))
        .collect();
    let mut sql = format!(
        "SELECT {key} AS \"{KEY_COLUMN}\", {} FROM {} t",
        list.join(", "),
        dialect.table_ref(schema, table)
    );
    if !conds.is_empty() {
        sql.push_str(&format!(" WHERE {}", conds.join(" AND ")));
    }
    sql.push_str(&format!(" ORDER BY {sort_key}"));
    dialect.push_page(&mut sql, crate::filter::TablePage::new(0, limit));
    Ok(sql)
}

/// Fallback for sources without a usable row key: plain offset paging in
/// whatever order the database returns.
pub fn offset_select_sql(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    columns: &[ColumnPlan],
    condition: Option<&str>,
    offset: usize,
    limit: usize,
) -> String {
    let list: Vec<String> = columns
        .iter()
        .map(|c| dialect.quote_ident(&c.source))
        .collect();
    let mut sql = format!(
        "SELECT {} FROM {}",
        list.join(", "),
        dialect.table_ref(schema, table)
    );
    if let Some(c) = condition {
        sql.push_str(&format!(" WHERE ({c})"));
    }
    let mut page = crate::filter::TablePage::new(offset, limit);
    page.limit = limit.max(1);
    dialect.push_page(&mut sql, page);
    sql
}

/// One value as a literal of the `target` database, for a column of
/// `column_type`. Errors name the problem, never the value.
pub fn literal(target: DbKind, column_type: &str, value: &Value) -> Result<String, String> {
    let dialect = target.dialect();
    match value {
        Value::Null => Ok("NULL".to_string()),
        Value::Clipped(c) => Err(format!(
            "a value of {} was cut by the fetch limit and cannot be copied",
            c.describe_len()
        )),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Float(f) if f.is_finite() => Ok(format!("{f}")),
        Value::Float(_) => Err("a NaN or infinite number cannot be copied".to_string()),
        Value::Bytes(b) => dialect.bytes_literal(b),
        Value::Text(s) => dialect.text_literal(column_type, s),
    }
}

/// One `INSERT` per row. `rows` hold the key first when `skip_key` is
/// set. Fails on the first value that cannot be written, with its row
/// number (1-based within `rows`) and column but never its content.
pub fn insert_statements(
    dialect: &dyn Dialect,
    schema: &str,
    table: &str,
    columns: &[ColumnPlan],
    rows: &[Vec<Value>],
    skip_key: bool,
    with_column_list: bool,
) -> Vec<Result<String, String>> {
    let target = dialect.table_ref(schema, table);
    let list = if with_column_list {
        let names: Vec<String> = columns
            .iter()
            .map(|c| dialect.quote_ident(&c.target))
            .collect();
        format!(" ({})", names.join(", "))
    } else {
        String::new()
    };
    let skip = usize::from(skip_key);
    rows.iter()
        .enumerate()
        .map(|(ix, row)| {
            if row.len() != columns.len() + skip {
                return Err(format!("row {}: unexpected number of columns", ix + 1));
            }
            let mut values = Vec::with_capacity(columns.len());
            for (col, value) in columns.iter().zip(&row[skip..]) {
                values.push(
                    literal(dialect.kind(), &col.target_type, value)
                        .map_err(|e| format!("row {}, column {}: {e}", ix + 1, col.source))?,
                );
            }
            Ok(format!(
                "INSERT INTO {target}{list} VALUES ({})",
                values.join(", ")
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{OracleDialect, SqliteDialect};

    fn col(name: &str, ty: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: ty.into(),
            nullable,
            default: None,
        }
    }

    #[test]
    fn types_map_between_engines() {
        use DbKind::*;
        assert_eq!(map_type(Oracle, Sqlite, "NUMBER(10)"), "INTEGER");
        assert_eq!(map_type(Oracle, Sqlite, "NUMBER(10,0)"), "INTEGER");
        assert_eq!(map_type(Oracle, Sqlite, "NUMBER(8,2)"), "NUMERIC");
        assert_eq!(map_type(Oracle, Sqlite, "NUMBER"), "NUMERIC");
        assert_eq!(map_type(Oracle, Sqlite, "VARCHAR2(20)"), "TEXT");
        assert_eq!(map_type(Oracle, Sqlite, "TIMESTAMP(6)"), "TEXT");
        assert_eq!(map_type(Oracle, Sqlite, "BLOB"), "BLOB");
        assert_eq!(map_type(Sqlite, Oracle, "INTEGER"), "NUMBER(19)");
        assert_eq!(map_type(Sqlite, Oracle, "text"), "VARCHAR2(4000)");
        assert_eq!(map_type(Sqlite, Oracle, ""), "VARCHAR2(4000)");
        assert_eq!(map_type(Sqlite, Oracle, "DATETIME"), "VARCHAR2(64)");
        assert_eq!(map_type(Sqlite, Oracle, "REAL"), "BINARY_DOUBLE");
        assert_eq!(map_type(Sqlite, Oracle, "DECIMAL(5,2)"), "NUMBER");
        assert_eq!(map_type(Oracle, Oracle, " NUMBER(8,2) "), "NUMBER(8,2)");
    }

    #[test]
    fn columns_follow_mapping_and_case() {
        let src = [
            col("Id", "NUMBER(10)", false),
            col("Name", "VARCHAR2(5)", true),
        ];
        let all = plan_columns(
            DbKind::Oracle,
            DbKind::Oracle,
            &src,
            &Fields::All,
            Some(NameCase::Lower),
        )
        .unwrap();
        assert_eq!(all[0].target, "id");
        assert!(!all[0].nullable);
        let custom = Fields::Custom(vec![super::super::FieldMap {
            source: "name".into(),
            target: "FULL".into(),
        }]);
        let c = plan_columns(DbKind::Oracle, DbKind::Oracle, &src, &custom, None).unwrap();
        assert_eq!(
            (c[0].source.as_str(), c[0].target.as_str()),
            ("Name", "FULL")
        );
        let missing = Fields::Custom(vec![super::super::FieldMap {
            source: "x".into(),
            target: "X".into(),
        }]);
        assert!(plan_columns(DbKind::Oracle, DbKind::Oracle, &src, &missing, None).is_err());
    }

    #[test]
    fn create_table_quotes_names_and_checks_types() {
        let cols = plan_columns(
            DbKind::Oracle,
            DbKind::Oracle,
            &[
                col("A\"B", "NUMBER(5)", false),
                col("C", "VARCHAR2(9)", true),
            ],
            &Fields::All,
            None,
        )
        .unwrap();
        assert_eq!(
            create_table_sql(&OracleDialect, "S", "T", &cols).unwrap(),
            "CREATE TABLE \"S\".\"T\" (\n  \"A\"\"B\" NUMBER(5) NOT NULL,\n  \"C\" VARCHAR2(9)\n)"
        );
        let mut bad = cols.clone();
        bad[0].target_type = "NUMBER); DROP TABLE X; --".into();
        assert!(create_table_sql(&OracleDialect, "S", "T", &bad).is_err());
    }

    #[test]
    fn indexes_skip_what_cannot_be_copied() {
        let cols = plan_columns(
            DbKind::Sqlite,
            DbKind::Sqlite,
            &[col("a", "INT", true), col("b", "TEXT", true)],
            &Fields::All,
            None,
        )
        .unwrap();
        let idx = |name: &str, kind: &str, columns: &[&str], unique| IndexDef {
            name: name.into(),
            unique,
            index_type: kind.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            status: "VALID".into(),
        };
        let d = &SqliteDialect;
        assert_eq!(
            create_index_sql(
                d,
                "main",
                "t",
                &idx("i1", "NORMAL", &["a", "b"], true),
                &cols,
                None
            )
            .unwrap(),
            "CREATE UNIQUE INDEX \"main\".\"i1\" ON \"t\" (\"a\", \"b\")"
        );
        assert!(create_index_sql(
            d,
            "main",
            "t",
            &idx("sqlite_autoindex_t_1", "NORMAL", &["a"], true),
            &cols,
            None
        )
        .is_none());
        assert!(create_index_sql(
            d,
            "main",
            "t",
            &idx("i", "FUNCTION-BASED NORMAL", &["a"], false),
            &cols,
            None
        )
        .is_none());
        assert!(create_index_sql(
            d,
            "main",
            "t",
            &idx("i", "NORMAL", &["zzz"], false),
            &cols,
            None
        )
        .is_none());
        assert_eq!(
            create_index_sql(
                &OracleDialect,
                "S",
                "T",
                &idx("I", "NORMAL", &["a"], false),
                &cols,
                None
            )
            .unwrap(),
            "CREATE INDEX \"S\".\"I\" ON \"S\".\"T\" (\"a\")"
        );
    }

    #[test]
    fn keyset_sql_is_quoted_and_paged() {
        let cols = plan_columns(
            DbKind::Oracle,
            DbKind::Oracle,
            &[col("A", "NUMBER", true)],
            &Fields::All,
            None,
        )
        .unwrap();
        let first = keyset_select_sql(
            &OracleDialect,
            "S",
            "T",
            &cols,
            Some("\"A\" > 1"),
            &Resume::Start,
            500,
        )
        .unwrap();
        assert_eq!(
            first,
            "SELECT t.ROWID AS \"__KEY__\", t.\"A\" FROM \"S\".\"T\" t WHERE (\"A\" > 1) ORDER BY t.ROWID FETCH FIRST 500 ROWS ONLY"
        );
        let next = keyset_select_sql(
            &OracleDialect,
            "S",
            "T",
            &cols,
            None,
            &Resume::After(Value::Text("AAAB+/c".into())),
            5,
        )
        .unwrap();
        assert!(next.contains("WHERE t.ROWID > CHARTOROWID('AAAB+/c')"));
        assert!(keyset_select_sql(
            &OracleDialect,
            "S",
            "T",
            &cols,
            None,
            &Resume::After(Value::Text("A'B".into())),
            5
        )
        .is_err());
        let sq = keyset_select_sql(
            &SqliteDialect,
            "main",
            "t",
            &cols,
            None,
            &Resume::After(Value::Integer(9)),
            5,
        )
        .unwrap();
        assert!(sq.contains("t.rowid > 9") && sq.ends_with("LIMIT 5"));
    }

    #[test]
    fn literals_are_typed_for_the_target() {
        let t = |s: &str| Value::Text(s.into());
        assert_eq!(
            literal(DbKind::Oracle, "VARCHAR2(9)", &t("O'Brien")).unwrap(),
            "'O''Brien'"
        );
        assert_eq!(
            literal(DbKind::Oracle, "NUMBER(8,2)", &t("12.50")).unwrap(),
            "12.50"
        );
        assert_eq!(
            literal(DbKind::Oracle, "NUMBER", &t("1; DROP")).unwrap(),
            "'1; DROP'"
        );
        assert_eq!(
            literal(DbKind::Oracle, "DATE", &t("2024-03-05 10:11:12")).unwrap(),
            "TO_DATE('2024-03-05 10:11:12', 'YYYY-MM-DD HH24:MI:SS')"
        );
        assert_eq!(
            literal(
                DbKind::Oracle,
                "TIMESTAMP(6)",
                &t("2024-03-05T10:11:12.123456")
            )
            .unwrap(),
            "TO_TIMESTAMP('2024-03-05 10:11:12.123456', 'YYYY-MM-DD HH24:MI:SS.FF')"
        );
        assert_eq!(
            literal(DbKind::Oracle, "DATE", &t("not a date")).unwrap(),
            "'not a date'"
        );
        assert_eq!(
            literal(DbKind::Oracle, "RAW(4)", &Value::Bytes(vec![0, 255])).unwrap(),
            "HEXTORAW('00FF')"
        );
        assert!(literal(DbKind::Oracle, "BLOB", &Value::Bytes(vec![0; 2001])).is_err());
        assert_eq!(
            literal(DbKind::Sqlite, "BLOB", &Value::Bytes(vec![1, 2])).unwrap(),
            "X'0102'"
        );
        assert_eq!(literal(DbKind::Sqlite, "INTEGER", &t("42")).unwrap(), "42");
        assert_eq!(literal(DbKind::Sqlite, "TEXT", &t("42")).unwrap(), "'42'");
        assert!(literal(DbKind::Sqlite, "TEXT", &t("a\0b")).is_err());
        assert!(literal(DbKind::Sqlite, "REAL", &Value::Float(f64::NAN)).is_err());
        assert_eq!(
            literal(DbKind::Sqlite, "TEXT", &Value::Null).unwrap(),
            "NULL"
        );
        let long = "é".repeat(3000);
        let lit = literal(DbKind::Oracle, "CLOB", &t(&long)).unwrap();
        assert!(lit.starts_with("TO_CLOB('") && lit.contains(" || TO_CLOB("));
    }

    #[test]
    fn clipped_cells_are_refused_without_naming_the_value() {
        let clipped = Value::Clipped(Box::new(crate::ClippedValue {
            preview: Value::Text("secret start".into()),
            full_len: 99,
            unit: crate::LenUnit::Bytes,
        }));
        let err = literal(DbKind::Sqlite, "TEXT", &clipped).unwrap_err();
        assert!(err.contains("cut by the fetch limit") && !err.contains("secret"));
    }

    #[test]
    fn inserts_skip_the_key_and_report_rows_not_values() {
        let cols = plan_columns(
            DbKind::Sqlite,
            DbKind::Sqlite,
            &[col("a", "INTEGER", true), col("b", "TEXT", true)],
            &Fields::All,
            None,
        )
        .unwrap();
        let rows = vec![
            vec![
                Value::Integer(1),
                Value::Integer(7),
                Value::Text("x".into()),
            ],
            vec![
                Value::Integer(2),
                Value::Integer(8),
                Value::Float(f64::INFINITY),
            ],
        ];
        let out = insert_statements(&SqliteDialect, "main", "t", &cols, &rows, true, true);
        assert_eq!(
            out[0].as_deref(),
            Ok("INSERT INTO \"main\".\"t\" (\"a\", \"b\") VALUES (7, 'x')")
        );
        assert!(out[1].as_ref().unwrap_err().starts_with("row 2, column b"));
        let bare = insert_statements(&SqliteDialect, "main", "t", &cols, &rows[..1], true, false);
        assert_eq!(
            bare[0].as_deref(),
            Ok("INSERT INTO \"main\".\"t\" VALUES (7, 'x')")
        );
    }
}
