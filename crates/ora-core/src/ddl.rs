//! Object operations of the explorer's right-click menus: create, rename,
//! duplicate, drop, empty and truncate a table, compile a program, reset a
//! sequence and call a function.
//!
//! Every identifier goes through [`Dialect::quote_ident`], so a name can
//! never close the quote and add SQL. Column types, which cannot be quoted,
//! are checked against a small character set by [`check_data_type`]. User
//! text that must appear as a literal (an external table's file name) is
//! quoted by [`quote_literal`].

use crate::client::{DbClient, DbError};
use crate::dialect::Dialect;
use crate::fetch::FetchLimits;
use crate::model::ObjectKind;

/// How a new table stores its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableKind {
    #[default]
    Normal,
    /// Rows read from a file in an Oracle directory (`ORACLE_LOADER`).
    External,
    /// Rows kept in the primary key's B-tree (`ORGANIZATION INDEX`).
    IndexOrganized,
}

impl TableKind {
    pub const ALL: [Self; 3] = [Self::Normal, Self::External, Self::IndexOrganized];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::External => "External",
            Self::IndexOrganized => "Index Organized",
        }
    }
}

/// One column of a new table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewColumn {
    pub name: String,
    /// Written as typed, e.g. `VARCHAR2(100)`; see [`check_data_type`].
    pub data_type: String,
    pub not_null: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTable {
    pub owner: String,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<NewColumn>,
    /// External tables: the Oracle directory object and the file in it.
    pub directory: String,
    pub location: String,
}

/// What a duplicate copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    StructureAndData,
    StructureOnly,
}

/// A parameter of a routine, from `ALL_ARGUMENTS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineArg {
    pub name: String,
    pub data_type: String,
    /// `IN`, `OUT` or `IN/OUT`.
    pub direction: String,
}

/// Work for the database thread. Each variant is one explorer action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectOp {
    /// Statements built by the functions below, run in order. `summary` is
    /// the message shown afterwards.
    Statements { sql: Vec<String>, summary: String },
    Compile {
        owner: String,
        name: String,
        kind: ObjectKind,
        debug: bool,
    },
    ResetSequence {
        owner: String,
        name: String,
        value: i128,
    },
    CallFunction {
        owner: String,
        name: String,
        args: Vec<String>,
    },
    /// Compile every INVALID object of the schema ("Compile Invalid").
    CompileInvalid { owner: String },
}

/// `'text'` with quotes doubled; line breaks are refused.
pub fn quote_literal(text: &str) -> Result<String, String> {
    if text.contains(['\n', '\r', '\0']) {
        return Err("The value must be one line.".to_string());
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

/// A column type may hold letters, digits, spaces, `_`, `(`, `)`, `,` and
/// `.` only: enough for `NUMBER(10,2)` or `TIMESTAMP(6) WITH TIME ZONE`,
/// and nothing that could end the statement or start a comment.
pub fn check_data_type(data_type: &str) -> Result<String, String> {
    let text = data_type.trim();
    if text.is_empty() {
        return Err("Every column needs a data type.".to_string());
    }
    if text.len() > 80
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '(' | ')' | ',' | '.'))
    {
        return Err(format!("'{text}' is not a valid data type."));
    }
    let depth_ok = text.chars().try_fold(0i32, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' if depth > 0 => Some(depth - 1),
        ')' => None,
        _ => Some(depth),
    }) == Some(0);
    if !depth_ok {
        return Err(format!("'{text}' has unbalanced parentheses."));
    }
    Ok(text.to_string())
}

fn check_name(what: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("Enter a {what} name."));
    }
    if name.contains('\0') || name.chars().any(char::is_control) {
        return Err(format!("The {what} name has a control character."));
    }
    // Oracle identifiers stop at 128 bytes (12.2+).
    if name.len() > 128 {
        return Err(format!("The {what} name is longer than 128 bytes."));
    }
    Ok(())
}

/// `CREATE TABLE` for `table`.
pub fn create_table_sql(dialect: &dyn Dialect, table: &NewTable) -> Result<String, String> {
    check_name("table", &table.name)?;
    if table.columns.is_empty() {
        return Err("Add at least one column.".to_string());
    }
    let external = table.kind == TableKind::External;
    let mut seen: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for column in &table.columns {
        check_name("column", &column.name)?;
        let lower = column.name.to_lowercase();
        if seen.contains(&lower) {
            return Err(format!("Column {} appears twice.", column.name));
        }
        seen.push(lower);
        let data_type = check_data_type(&column.data_type)?;
        let mut line = format!("{} {data_type}", dialect.quote_ident(&column.name));
        // External tables accept no constraints.
        if !external {
            if column.not_null || column.primary_key {
                line.push_str(" NOT NULL");
            }
            if column.primary_key {
                keys.push(dialect.quote_ident(&column.name));
            }
        }
        lines.push(line);
    }
    if table.kind == TableKind::IndexOrganized && keys.is_empty() {
        return Err("An index-organized table needs a primary key column.".to_string());
    }
    if !keys.is_empty() {
        lines.push(format!("PRIMARY KEY ({})", keys.join(", ")));
    }
    let mut sql = format!(
        "CREATE TABLE {} (\n  {}\n)",
        dialect.table_ref(&table.owner, &table.name),
        lines.join(",\n  ")
    );
    match table.kind {
        TableKind::Normal => {}
        TableKind::IndexOrganized => sql.push_str("\nORGANIZATION INDEX"),
        TableKind::External => {
            if table.directory.trim().is_empty() {
                return Err("Enter the directory object of the external file.".to_string());
            }
            if table.location.trim().is_empty() {
                return Err("Enter the file name of the external table.".to_string());
            }
            sql.push_str(&format!(
                "\nORGANIZATION EXTERNAL (\n  TYPE ORACLE_LOADER\n  DEFAULT DIRECTORY {}\n  \
                 ACCESS PARAMETERS (RECORDS DELIMITED BY NEWLINE FIELDS TERMINATED BY ',' \
                 MISSING FIELD VALUES ARE NULL)\n  LOCATION ({})\n)\nREJECT LIMIT UNLIMITED",
                dialect.quote_ident(table.directory.trim()),
                quote_literal(table.location.trim())?
            ));
        }
    }
    Ok(sql)
}

pub fn rename_table_sql(
    dialect: &dyn Dialect,
    owner: &str,
    old: &str,
    new: &str,
) -> Result<String, String> {
    check_name("table", new)?;
    if new == old {
        return Err("The new name is the same as the old one.".to_string());
    }
    Ok(dialect.rename_table(owner, old, new))
}

/// `CREATE TABLE ... AS SELECT`. Copies columns and rows only: indexes,
/// constraints and triggers of the source are not copied.
pub fn duplicate_table_sql(
    dialect: &dyn Dialect,
    owner: &str,
    source: &str,
    new: &str,
    mode: CopyMode,
) -> Result<String, String> {
    check_name("table", new)?;
    if new == source {
        return Err("The copy needs a different name.".to_string());
    }
    let filter = match mode {
        CopyMode::StructureAndData => "",
        CopyMode::StructureOnly => " WHERE 1 = 0",
    };
    Ok(format!(
        "CREATE TABLE {} AS SELECT * FROM {}{filter}",
        dialect.table_ref(owner, new),
        dialect.table_ref(owner, source)
    ))
}

pub fn drop_table_sql(dialect: &dyn Dialect, owner: &str, name: &str) -> String {
    format!("DROP TABLE {}", dialect.table_ref(owner, name))
}

/// `DELETE FROM`: row by row, can be rolled back.
pub fn empty_table_sql(dialect: &dyn Dialect, owner: &str, name: &str) -> String {
    format!("DELETE FROM {}", dialect.table_ref(owner, name))
}

/// `TRUNCATE TABLE`: fast, commits by itself, cannot be rolled back.
pub fn truncate_table_sql(dialect: &dyn Dialect, owner: &str, name: &str) -> String {
    format!("TRUNCATE TABLE {}", dialect.table_ref(owner, name))
}

/// Lower-case name of an object kind, for messages.
pub fn kind_noun(kind: &ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "materialized view",
        ObjectKind::Index => "index",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Function => "function",
        ObjectKind::Package => "package",
        ObjectKind::Trigger => "trigger",
        ObjectKind::Synonym => "synonym",
        ObjectKind::Type => "type",
        ObjectKind::DbLink => "database link",
        ObjectKind::Constraint => "constraint",
        ObjectKind::RecycleBin => "recycle bin object",
        ObjectKind::Other(_) => "object",
    }
}

/// `DROP <kind> owner.name` for the kinds the explorer can delete.
pub fn drop_object_sql(
    dialect: &dyn Dialect,
    owner: &str,
    name: &str,
    kind: &ObjectKind,
    parent: Option<&str>,
) -> Option<String> {
    let noun = match kind {
        ObjectKind::Table
        | ObjectKind::View
        | ObjectKind::MaterializedView
        | ObjectKind::Index
        | ObjectKind::Sequence
        | ObjectKind::Procedure
        | ObjectKind::Function
        | ObjectKind::Package
        | ObjectKind::Trigger
        | ObjectKind::Synonym
        | ObjectKind::Type => kind_noun(kind).to_ascii_uppercase(),
        _ => return None,
    };
    dialect.drop_object(&noun, owner, name, parent)
}

/// `ALTER TABLE ... ENABLE|DISABLE CONSTRAINT`.
pub fn constraint_state_sql(
    dialect: &dyn Dialect,
    owner: &str,
    table: &str,
    name: &str,
    enable: bool,
) -> String {
    format!(
        "ALTER TABLE {} {} CONSTRAINT {}",
        dialect.table_ref(owner, table),
        if enable { "ENABLE" } else { "DISABLE" },
        dialect.quote_ident(name)
    )
}

pub fn drop_constraint_sql(dialect: &dyn Dialect, owner: &str, table: &str, name: &str) -> String {
    format!(
        "ALTER TABLE {} DROP CONSTRAINT {}",
        dialect.table_ref(owner, table),
        dialect.quote_ident(name)
    )
}

/// Statement that recompiles one INVALID object of dictionary type
/// `object_type` (`PACKAGE BODY` and `TYPE BODY` included).
pub fn compile_invalid_sql(
    dialect: &dyn Dialect,
    owner: &str,
    name: &str,
    object_type: &str,
) -> Option<String> {
    let target = dialect.table_ref(owner, name);
    match object_type {
        "PACKAGE BODY" => Some(format!("ALTER PACKAGE {target} COMPILE BODY")),
        "TYPE BODY" => Some(format!("ALTER TYPE {target} COMPILE BODY")),
        other => compile_sql(
            dialect,
            owner,
            name,
            &ObjectKind::from_oracle_type(other),
            false,
        ),
    }
}

/// `ALTER TRIGGER ... ENABLE|DISABLE`.
pub fn trigger_state_sql(dialect: &dyn Dialect, owner: &str, name: &str, enable: bool) -> String {
    format!(
        "ALTER TRIGGER {} {}",
        dialect.table_ref(owner, name),
        if enable { "ENABLE" } else { "DISABLE" }
    )
}

/// `ALTER ... COMPILE [DEBUG]`, or `None` for an object that cannot be
/// compiled (or has no debug mode).
pub fn compile_sql(
    dialect: &dyn Dialect,
    owner: &str,
    name: &str,
    kind: &ObjectKind,
    debug: bool,
) -> Option<String> {
    let (noun, has_debug) = match kind {
        ObjectKind::Procedure => ("PROCEDURE", true),
        ObjectKind::Function => ("FUNCTION", true),
        ObjectKind::Package => ("PACKAGE", true),
        ObjectKind::Trigger => ("TRIGGER", true),
        ObjectKind::Type => ("TYPE", true),
        ObjectKind::View => ("VIEW", false),
        ObjectKind::MaterializedView => ("MATERIALIZED VIEW", false),
        ObjectKind::Synonym => ("SYNONYM", false),
        _ => return None,
    };
    if debug && !has_debug {
        return None;
    }
    Some(format!(
        "ALTER {noun} {} COMPILE{}",
        dialect.table_ref(owner, name),
        if debug { " DEBUG" } else { "" }
    ))
}

/// Whether the explorer offers Compile for this object kind, and whether
/// "Compile for Debug" too.
pub fn compile_support(kind: &ObjectKind) -> (bool, bool) {
    let normal = compile_sql(&crate::dialect::OracleDialect, "O", "N", kind, false).is_some();
    let debug = compile_sql(&crate::dialect::OracleDialect, "O", "N", kind, true).is_some();
    (normal, debug)
}

/// `ALTER SEQUENCE ... RESTART START WITH n` (Oracle 18c and later).
pub fn restart_sequence_sql(dialect: &dyn Dialect, owner: &str, name: &str, value: i128) -> String {
    format!(
        "ALTER SEQUENCE {} RESTART START WITH {value}",
        dialect.table_ref(owner, name)
    )
}

fn alter_increment_sql(dialect: &dyn Dialect, owner: &str, name: &str, step: i128) -> String {
    format!(
        "ALTER SEQUENCE {} INCREMENT BY {step}",
        dialect.table_ref(owner, name)
    )
}

/// `SELECT "OWNER"."FUNC"(:1, :2, ...) FROM DUAL`.
pub fn call_function_sql(dialect: &dyn Dialect, owner: &str, name: &str, args: usize) -> String {
    let binds: Vec<String> = (1..=args).map(|n| format!(":{n}")).collect();
    format!(
        "SELECT {}({}) AS RESULT FROM DUAL",
        dialect.table_ref(owner, name),
        binds.join(", ")
    )
}

/// Why a function cannot be run from a `SELECT`, if it cannot: an OUT
/// parameter, or a PL/SQL-only type.
pub fn call_blocker(args: &[RoutineArg], return_type: &str) -> Option<String> {
    let plsql_only = |t: &str| {
        let t = t.to_ascii_uppercase();
        t.contains("BOOLEAN")
            || t.contains("RECORD")
            || t.contains("TABLE")
            || t.contains("REF CURSOR")
            || t.contains("PL/SQL")
    };
    if let Some(a) = args.iter().find(|a| a.direction != "IN") {
        return Some(format!(
            "Parameter {} is {}; this dialog only runs functions with IN parameters. Use the SQL editor.",
            a.name, a.direction
        ));
    }
    if let Some(a) = args.iter().find(|a| plsql_only(&a.data_type)) {
        return Some(format!(
            "Parameter {} is {}, which a query cannot pass. Use the SQL editor.",
            a.name, a.data_type
        ));
    }
    if plsql_only(return_type) {
        return Some(format!(
            "The function returns {return_type}, which a query cannot read. Use the SQL editor."
        ));
    }
    None
}

/// Run `op` on `client`; the lines are for the Messages panel.
pub fn run(client: &mut dyn DbClient, op: &ObjectOp) -> Result<Vec<String>, DbError> {
    match op {
        ObjectOp::Statements { sql, summary } => {
            for statement in sql {
                client.execute(statement)?;
            }
            Ok(vec![summary.clone()])
        }
        ObjectOp::Compile {
            owner,
            name,
            kind,
            debug,
        } => {
            let sql = compile_sql(client.dialect(), owner, name, kind, *debug)
                .ok_or_else(|| DbError::new("This object cannot be compiled."))?;
            client.execute(&sql)?;
            let mut lines = Vec::new();
            let errors = client.query_binds(
                "SELECT attribute, line, position, text FROM all_errors \
                 WHERE owner = :1 AND name = :2 ORDER BY type, sequence",
                &[owner.to_ascii_uppercase(), name.clone()],
                FetchLimits::rows(200),
            )?;
            for row in &errors.rows {
                let cell = |i: usize| row.get(i).map(|v| v.display()).unwrap_or_default();
                lines.push(format!(
                    "  {} at line {}, column {}: {}",
                    cell(0).to_lowercase(),
                    cell(1),
                    cell(2),
                    cell(3).trim_end()
                ));
            }
            let what = if *debug {
                "Compiled for debug"
            } else {
                "Compiled"
            };
            lines.insert(
                0,
                if lines.is_empty() {
                    format!("{what} {owner}.{name}.")
                } else {
                    format!("{what} {owner}.{name} with errors:")
                },
            );
            Ok(lines)
        }
        ObjectOp::ResetSequence { owner, name, value } => {
            reset_sequence(client, owner, name, *value)
        }
        ObjectOp::CompileInvalid { owner } => compile_invalid(client, owner),
        ObjectOp::CallFunction { owner, name, args } => {
            let sql = call_function_sql(client.dialect(), owner, name, args.len());
            let result = client.query_binds(&sql, args, FetchLimits::rows(1))?;
            let shown = result
                .rows
                .first()
                .and_then(|r| r.first())
                .map(|v| {
                    if v.is_null() {
                        "NULL".to_string()
                    } else {
                        v.display()
                    }
                })
                .unwrap_or_else(|| "(no value)".to_string());
            Ok(vec![
                format!("{owner}.{name}({}) returned:", args.join(", ")),
                format!("  {shown}"),
            ])
        }
    }
}

fn invalid_objects(
    client: &mut dyn DbClient,
    owner: &str,
) -> Result<Vec<(String, String)>, DbError> {
    let result = client.query_binds(
        "SELECT object_name, object_type FROM all_objects \
         WHERE owner = :1 AND status = 'INVALID' \
         AND object_type IN ('PROCEDURE','FUNCTION','PACKAGE','PACKAGE BODY','TRIGGER','VIEW',\
                             'TYPE','TYPE BODY','SYNONYM','MATERIALIZED VIEW') \
         ORDER BY object_type, object_name",
        &[owner.to_ascii_uppercase()],
        FetchLimits::rows(2000),
    )?;
    Ok(result
        .rows
        .iter()
        .map(|r| (r[0].display(), r[1].display()))
        .collect())
}

/// Recompile the schema's INVALID objects. Objects depend on each other,
/// so it makes up to four passes, as long as a pass fixes something.
fn compile_invalid(client: &mut dyn DbClient, owner: &str) -> Result<Vec<String>, DbError> {
    let mut remaining = invalid_objects(client, owner)?;
    let total = remaining.len();
    if total == 0 {
        return Ok(vec![format!("No invalid objects in {owner}.")]);
    }
    let mut failures: Vec<String> = Vec::new();
    for _ in 0..4 {
        let before = remaining.len();
        failures.clear();
        for (name, object_type) in &remaining {
            let Some(sql) = compile_invalid_sql(client.dialect(), owner, name, object_type) else {
                continue;
            };
            if let Err(err) = client.execute(&sql) {
                failures.push(format!("  {object_type} {name}: {err}"));
            }
        }
        remaining = invalid_objects(client, owner)?;
        if remaining.is_empty() || remaining.len() >= before {
            break;
        }
    }
    let mut lines = vec![format!(
        "Compiled {} of {total} invalid objects in {owner}.",
        total - remaining.len()
    )];
    if !remaining.is_empty() {
        lines.push(format!("Still invalid ({}):", remaining.len()));
        let errors = client
            .query_binds(
                "SELECT name, type, text FROM all_errors \
                 WHERE owner = :1 AND attribute = 'ERROR' AND sequence = 1",
                &[owner.to_ascii_uppercase()],
                FetchLimits::rows(2000),
            )
            .map(|r| r.rows)
            .unwrap_or_default();
        for (name, object_type) in remaining.iter().take(30) {
            let why = errors
                .iter()
                .find(|r| r[0].display() == *name && r[1].display() == *object_type)
                .map(|r| r[2].display().trim().to_string())
                .unwrap_or_default();
            lines.push(
                format!("  {object_type} {name} {why}")
                    .trim_end()
                    .to_string(),
            );
        }
        if remaining.len() > 30 {
            lines.push(format!("  ... and {} more", remaining.len() - 30));
        }
    }
    lines.extend(failures.into_iter().take(10));
    Ok(lines)
}

/// Restart a sequence so the next `NEXTVAL` is `value`. Oracle 18c has
/// `RESTART`; older servers answer ORA-00933/ORA-02248 and get the classic
/// way: step the sequence to `value - increment`, then put the increment
/// back.
fn reset_sequence(
    client: &mut dyn DbClient,
    owner: &str,
    name: &str,
    value: i128,
) -> Result<Vec<String>, DbError> {
    let restart = restart_sequence_sql(client.dialect(), owner, name, value);
    match client.execute(&restart) {
        Ok(_) => {
            return Ok(vec![format!(
                "Reset {owner}.{name}: the next value is {value}."
            )])
        }
        Err(err) => {
            let text = err.to_string();
            if !(text.contains("ORA-00933") || text.contains("ORA-02248")) {
                return Err(err);
            }
        }
    }
    let info = client.query_binds(
        "SELECT increment_by FROM all_sequences WHERE sequence_owner = :1 AND sequence_name = :2",
        &[owner.to_ascii_uppercase(), name.to_string()],
        FetchLimits::rows(1),
    )?;
    let step: i128 = info
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.display().trim().parse().ok())
        .ok_or_else(|| DbError::new("The sequence was not found."))?;
    let current = client.query(
        &format!(
            "SELECT {}.NEXTVAL FROM DUAL",
            client.dialect().table_ref(owner, name)
        ),
        FetchLimits::rows(1),
    )?;
    let current: i128 = current
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.display().trim().parse().ok())
        .ok_or_else(|| DbError::new("The sequence returned no value."))?;
    let jump = value - step - current;
    if jump != 0 {
        client.execute(&alter_increment_sql(client.dialect(), owner, name, jump))?;
        let stepped = client.query(
            &format!(
                "SELECT {}.NEXTVAL FROM DUAL",
                client.dialect().table_ref(owner, name)
            ),
            FetchLimits::rows(1),
        );
        // Always put the increment back, even when the step failed.
        let restore = client.execute(&alter_increment_sql(client.dialect(), owner, name, step));
        stepped?;
        restore?;
    }
    Ok(vec![format!(
        "Reset {owner}.{name} (older Oracle, increment trick): the next value is {value}."
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{OracleDialect, SqliteDialect};

    fn col(name: &str, data_type: &str, pk: bool) -> NewColumn {
        NewColumn {
            name: name.into(),
            data_type: data_type.into(),
            not_null: false,
            primary_key: pk,
        }
    }

    fn table(kind: TableKind) -> NewTable {
        NewTable {
            owner: "HR".into(),
            name: "T".into(),
            kind,
            columns: vec![
                col("ID", "NUMBER(10)", true),
                col("NAME", "VARCHAR2(50)", false),
            ],
            ..NewTable::default()
        }
    }

    #[test]
    fn create_normal_table() {
        let sql = create_table_sql(&OracleDialect, &table(TableKind::Normal)).unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE \"HR\".\"T\" (\n  \"ID\" NUMBER(10) NOT NULL,\n  \"NAME\" VARCHAR2(50),\n  PRIMARY KEY (\"ID\")\n)"
        );
    }

    #[test]
    fn create_index_organized_needs_key() {
        let sql = create_table_sql(&OracleDialect, &table(TableKind::IndexOrganized)).unwrap();
        assert!(sql.ends_with("\nORGANIZATION INDEX"));
        let mut t = table(TableKind::IndexOrganized);
        t.columns[0].primary_key = false;
        assert!(create_table_sql(&OracleDialect, &t).is_err());
    }

    #[test]
    fn create_external_quotes_directory_and_file() {
        let mut t = table(TableKind::External);
        t.directory = "DATA_DIR".into();
        t.location = "it's.csv".into();
        let sql = create_table_sql(&OracleDialect, &t).unwrap();
        assert!(sql.contains("DEFAULT DIRECTORY \"DATA_DIR\""));
        assert!(sql.contains("LOCATION ('it''s.csv')"));
        assert!(
            !sql.contains("PRIMARY KEY"),
            "external tables take no constraints"
        );
        assert!(!sql.contains("NOT NULL"));
        t.directory.clear();
        assert!(create_table_sql(&OracleDialect, &t).is_err());
    }

    #[test]
    fn names_cannot_break_out_of_quotes() {
        let sql =
            rename_table_sql(&OracleDialect, "HR", "A", "B\" CASCADE; DROP TABLE X --").unwrap();
        assert_eq!(
            sql,
            "ALTER TABLE \"HR\".\"A\" RENAME TO \"B\"\" CASCADE; DROP TABLE X --\""
        );
        assert!(drop_table_sql(&SqliteDialect, "main", "a\"b").contains("\"a\"\"b\""));
    }

    #[test]
    fn data_types_are_checked() {
        assert!(check_data_type("NUMBER(10,2)").is_ok());
        assert!(check_data_type("TIMESTAMP(6) WITH TIME ZONE").is_ok());
        for bad in [
            "",
            "NUMBER; DROP TABLE X",
            "VARCHAR2(5))",
            "A--B",
            "X'Y",
            "((",
            "A\nB",
        ] {
            assert!(check_data_type(bad).is_err(), "{bad:?}");
        }
        let mut t = table(TableKind::Normal);
        t.columns[1].data_type = "INT); DROP TABLE X; --".into();
        assert!(create_table_sql(&OracleDialect, &t).is_err());
    }

    #[test]
    fn duplicate_modes() {
        let all = duplicate_table_sql(&OracleDialect, "HR", "A", "B", CopyMode::StructureAndData)
            .unwrap();
        assert_eq!(
            all,
            "CREATE TABLE \"HR\".\"B\" AS SELECT * FROM \"HR\".\"A\""
        );
        let shape =
            duplicate_table_sql(&OracleDialect, "HR", "A", "B", CopyMode::StructureOnly).unwrap();
        assert!(shape.ends_with("WHERE 1 = 0"));
        assert!(
            duplicate_table_sql(&OracleDialect, "HR", "A", "A", CopyMode::StructureOnly).is_err()
        );
    }

    #[test]
    fn compile_statements() {
        let o = &OracleDialect;
        assert_eq!(
            compile_sql(o, "HR", "P", &ObjectKind::Procedure, true).unwrap(),
            "ALTER PROCEDURE \"HR\".\"P\" COMPILE DEBUG"
        );
        assert_eq!(
            compile_sql(o, "HR", "V", &ObjectKind::View, false).unwrap(),
            "ALTER VIEW \"HR\".\"V\" COMPILE"
        );
        assert!(compile_sql(o, "HR", "V", &ObjectKind::View, true).is_none());
        assert!(compile_sql(o, "HR", "T", &ObjectKind::Table, false).is_none());
        assert_eq!(compile_support(&ObjectKind::Package), (true, true));
        assert_eq!(compile_support(&ObjectKind::Synonym), (true, false));
    }

    #[test]
    fn drop_and_constraint_statements() {
        let o = &OracleDialect;
        assert_eq!(
            drop_object_sql(o, "HR", "V", &ObjectKind::MaterializedView, None).unwrap(),
            "DROP MATERIALIZED VIEW \"HR\".\"V\""
        );
        assert!(drop_object_sql(o, "HR", "L", &ObjectKind::DbLink, None).is_none());
        assert_eq!(
            constraint_state_sql(o, "HR", "T", "C\"1", false),
            "ALTER TABLE \"HR\".\"T\" DISABLE CONSTRAINT \"C\"\"1\""
        );
        assert_eq!(
            drop_constraint_sql(o, "HR", "T", "C1"),
            "ALTER TABLE \"HR\".\"T\" DROP CONSTRAINT \"C1\""
        );
        assert_eq!(
            compile_invalid_sql(o, "HR", "P", "PACKAGE BODY").unwrap(),
            "ALTER PACKAGE \"HR\".\"P\" COMPILE BODY"
        );
        assert!(compile_invalid_sql(o, "HR", "J", "JAVA CLASS").is_none());
    }

    #[test]
    fn trigger_state() {
        assert_eq!(
            trigger_state_sql(&OracleDialect, "HR", "T", false),
            "ALTER TRIGGER \"HR\".\"T\" DISABLE"
        );
    }

    #[test]
    fn sequence_and_function_sql() {
        assert_eq!(
            restart_sequence_sql(&OracleDialect, "HR", "S", 1),
            "ALTER SEQUENCE \"HR\".\"S\" RESTART START WITH 1"
        );
        assert_eq!(
            call_function_sql(&OracleDialect, "HR", "F", 2),
            "SELECT \"HR\".\"F\"(:1, :2) AS RESULT FROM DUAL"
        );
        assert_eq!(
            call_function_sql(&OracleDialect, "HR", "F", 0),
            "SELECT \"HR\".\"F\"() AS RESULT FROM DUAL"
        );
    }

    #[test]
    fn call_blockers() {
        let arg = |d: &str, t: &str| RoutineArg {
            name: "P".into(),
            data_type: t.into(),
            direction: d.into(),
        };
        assert!(call_blocker(&[arg("IN", "NUMBER")], "VARCHAR2").is_none());
        assert!(call_blocker(&[arg("OUT", "NUMBER")], "VARCHAR2").is_some());
        assert!(call_blocker(&[arg("IN", "PL/SQL BOOLEAN")], "NUMBER").is_some());
        assert!(call_blocker(&[], "REF CURSOR").is_some());
    }
}
