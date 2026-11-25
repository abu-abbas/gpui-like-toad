//! Fast DDL for the DDL tab and View Script, read from the data dictionary.
//!
//! `DBMS_METADATA.GET_DDL` is slow the first time a session asks for an
//! object: the server parses and runs dozens of heavy `KU$` view queries.
//! On a large production dictionary that first call took 9-16 s (timing.log,
//! even at 7 ms ping), while the same object took ~250 ms afterwards. The
//! dictionary views used here (`ALL_TAB_COLS`, `ALL_CONSTRAINTS`,
//! `ALL_SOURCE`, ...) answer in a few round trips, which is what tools like
//! Toad do as well.
//!
//! Only plain cases are built here. Anything this module does not fully
//! understand (partitioned, index-organized, nested, clustered or external
//! tables, identity or vector columns, object views, missing dictionary
//! rows, a query error on an older server) returns `None`, and the caller
//! falls back to `DBMS_METADATA`. "Show storage" always uses
//! `DBMS_METADATA`.

use ora_core::ObjectKind;

type Conn = oracledb::Connection;
type Res<T> = Result<T, oracledb::Error>;

/// DDL for `owner.name` from the dictionary, or `None` when the object
/// needs `DBMS_METADATA` (see the module docs). `None` also covers query
/// errors: the caller's `DBMS_METADATA` call reports a real problem.
pub(crate) fn native_ddl(
    conn: &Conn,
    owner: &str,
    name: &str,
    kind: &ObjectKind,
) -> Option<String> {
    // The column flags read below (IDENTITY_COLUMN, USER_GENERATED, ...)
    // need 12c.
    if conn.version().map_or(true, |v| v.0 < 12) {
        return None;
    }
    let result = match kind {
        ObjectKind::Table => table_ddl(conn, owner, name),
        ObjectKind::View => view_ddl(conn, owner, name),
        ObjectKind::Index => index_ddl(conn, owner, name),
        ObjectKind::Sequence => sequence_ddl(conn, owner, name),
        ObjectKind::Synonym => synonym_ddl(conn, owner, name),
        ObjectKind::Procedure => source_ddl(conn, owner, name, &["PROCEDURE"]),
        ObjectKind::Function => source_ddl(conn, owner, name, &["FUNCTION"]),
        ObjectKind::Package => source_ddl(conn, owner, name, &["PACKAGE", "PACKAGE BODY"]),
        ObjectKind::Type => source_ddl(conn, owner, name, &["TYPE", "TYPE BODY"]),
        ObjectKind::Trigger => trigger_ddl(conn, owner, name),
        _ => Ok(None),
    };
    result.ok().flatten()
}

/// `"NAME"` with inner quotes doubled.
fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `"OWNER"."NAME"`.
fn qn(owner: &str, name: &str) -> String {
    format!("{}.{}", q(owner), q(name))
}

/// `'text'` with inner quotes doubled.
fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Rows of a dictionary query, every column read as optional text (the
/// SQL turns numbers into text with `TO_CHAR`).
fn text_rows(conn: &Conn, sql: &str, binds: &[&str]) -> Res<Vec<Vec<Option<String>>>> {
    let params: Vec<&dyn oracledb::ToDbValue> = binds
        .iter()
        .map(|b| b as &dyn oracledb::ToDbValue)
        .collect();
    let cursor = conn.query(sql, &params)?;
    let width = cursor.columns().len();
    let mut out = Vec::new();
    for row in cursor {
        let row = row?;
        let mut cells = Vec::with_capacity(width);
        for ix in 0..width {
            cells.push(row.get::<Option<String>>(ix)?);
        }
        out.push(cells);
    }
    Ok(out)
}

/// Cell `ix` of a [`text_rows`] row, `""` for NULL.
fn cell(row: &[Option<String>], ix: usize) -> &str {
    row.get(ix).and_then(|c| c.as_deref()).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// One column of a table, as `ALL_TAB_COLS` describes it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Col {
    pub name: String,
    pub data_type: String,
    pub type_owner: String,
    pub data_length: String,
    pub char_length: String,
    pub char_used: String,
    pub precision: String,
    pub scale: String,
    pub default: String,
    pub default_on_null: bool,
    pub virtual_col: bool,
    pub invisible: bool,
    /// `GENERATED ... AS IDENTITY (...)` for an identity column, else empty.
    pub identity: String,
    pub comment: String,
}

/// The SQL type of `col` as written in a CREATE TABLE.
pub(crate) fn column_type(col: &Col) -> String {
    let t = col.data_type.as_str();
    let builtin_owner = col.type_owner.is_empty()
        || col.type_owner.eq_ignore_ascii_case("SYS")
        || col.type_owner.eq_ignore_ascii_case("PUBLIC");
    if !builtin_owner {
        return qn(&col.type_owner, t);
    }
    let chars = |len: &str| {
        if col.char_used == "C" {
            format!("{t}({len} CHAR)")
        } else {
            format!("{t}({len})")
        }
    };
    match t {
        "VARCHAR2" | "CHAR" => chars(if col.char_used == "C" {
            &col.char_length
        } else {
            &col.data_length
        }),
        "NVARCHAR2" | "NCHAR" => format!("{t}({})", col.char_length),
        "RAW" | "UROWID" => format!("{t}({})", col.data_length),
        "FLOAT" if !col.precision.is_empty() => format!("FLOAT({})", col.precision),
        "NUMBER" => match (col.precision.as_str(), col.scale.as_str()) {
            ("", "") => "NUMBER".to_string(),
            ("", s) => format!("NUMBER(*,{s})"),
            (p, "") => format!("NUMBER({p})"),
            (p, s) => format!("NUMBER({p},{s})"),
        },
        other => other.to_string(),
    }
}

/// `GENERATED BY DEFAULT AS IDENTITY (START WITH 1 ...)` from
/// `ALL_TAB_IDENTITY_COLS` (`generation` and `options`, e.g.
/// `START WITH: 1, INCREMENT BY: 1, MAX_VALUE: ..., CACHE_SIZE: 20, ...`).
pub(crate) fn identity_clause(generation: &str, on_null: bool, options: &str) -> String {
    let opts: Vec<(&str, &str)> = options
        .split(", ")
        .filter_map(|part| part.split_once(": "))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let get = |key: &str| opts.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let mut parts = Vec::new();
    if let Some(v) = get("START WITH") {
        parts.push(format!("START WITH {v}"));
    }
    if let Some(v) = get("INCREMENT BY") {
        parts.push(format!("INCREMENT BY {v}"));
    }
    if let Some(v) = get("MIN_VALUE") {
        parts.push(format!("MINVALUE {v}"));
    }
    if let Some(v) = get("MAX_VALUE") {
        parts.push(format!("MAXVALUE {v}"));
    }
    match get("CACHE_SIZE") {
        Some("0") => parts.push("NOCACHE".into()),
        Some(v) => parts.push(format!("CACHE {v}")),
        None => {}
    }
    if get("CYCLE_FLAG") == Some("Y") {
        parts.push("CYCLE".into());
    }
    if get("ORDER_FLAG") == Some("Y") {
        parts.push("ORDER".into());
    }
    let generation = if generation == "ALWAYS" {
        "ALWAYS"
    } else if on_null {
        "BY DEFAULT ON NULL"
    } else {
        "BY DEFAULT"
    };
    if parts.is_empty() {
        format!("GENERATED {generation} AS IDENTITY")
    } else {
        format!("GENERATED {generation} AS IDENTITY ({})", parts.join(" "))
    }
}

/// One line of the column list, without the trailing comma. `not_null`
/// is the column's `"COL" IS NOT NULL` check, if it has one: a primary
/// key column is NOT NULL without one, and says so only through the key.
pub(crate) fn column_line(col: &Col, not_null: Option<&Cons>) -> String {
    let mut line = format!("  {} {}", q(&col.name), column_type(col));
    if col.invisible {
        line.push_str(" INVISIBLE");
    }
    let default = col.default.trim();
    if !col.identity.is_empty() {
        line.push(' ');
        line.push_str(&col.identity);
    } else if col.virtual_col {
        line.push_str(&format!(" GENERATED ALWAYS AS ({default}) VIRTUAL"));
    } else if !default.is_empty() {
        if col.default_on_null {
            line.push_str(&format!(" DEFAULT ON NULL {default}"));
        } else {
            line.push_str(&format!(" DEFAULT {default}"));
        }
    }
    // Identity and DEFAULT ON NULL columns get theirs implicitly.
    if let Some(c) = not_null.filter(|_| col.identity.is_empty() && !col.default_on_null) {
        if !c.generated_name {
            line.push_str(&format!(" CONSTRAINT {}", q(&c.name)));
        }
        line.push_str(" NOT NULL");
        if !c.enabled {
            line.push_str(" DISABLE");
        }
    }
    line
}

/// One constraint of a table, columns in key order.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Cons {
    pub name: String,
    /// `P`, `U`, `R` or `C`.
    pub kind: String,
    pub generated_name: bool,
    pub condition: String,
    pub delete_rule: String,
    pub enabled: bool,
    pub deferrable: bool,
    pub deferred: bool,
    pub validated: bool,
    pub columns: Vec<String>,
    pub ref_owner: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// Index behind a primary/unique key: not listed again on its own.
    pub index_owner: String,
    pub index_name: String,
}

impl Cons {
    /// A `"COL" IS NOT NULL` check: the column's NOT NULL already says it.
    fn is_not_null_check(&self) -> bool {
        if self.kind != "C" || self.columns.len() != 1 {
            return false;
        }
        self.condition.trim() == format!("{} IS NOT NULL", q(&self.columns[0]))
    }
}

/// One line of the constraint part of the column list.
pub(crate) fn constraint_line(c: &Cons, table_owner: &str) -> String {
    let mut line = String::from("  ");
    if !c.generated_name {
        line.push_str(&format!("CONSTRAINT {} ", q(&c.name)));
    }
    let cols = |names: &[String]| names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", ");
    match c.kind.as_str() {
        "P" => line.push_str(&format!("PRIMARY KEY ({})", cols(&c.columns))),
        "U" => line.push_str(&format!("UNIQUE ({})", cols(&c.columns))),
        "R" => {
            let target = if c.ref_owner.is_empty() || c.ref_owner == table_owner {
                q(&c.ref_table)
            } else {
                qn(&c.ref_owner, &c.ref_table)
            };
            line.push_str(&format!(
                "FOREIGN KEY ({}) REFERENCES {target} ({})",
                cols(&c.columns),
                cols(&c.ref_columns)
            ));
            match c.delete_rule.as_str() {
                "CASCADE" => line.push_str(" ON DELETE CASCADE"),
                "SET NULL" => line.push_str(" ON DELETE SET NULL"),
                _ => {}
            }
        }
        _ => line.push_str(&format!("CHECK ({})", c.condition.trim())),
    }
    if c.deferrable {
        line.push_str(if c.deferred {
            " DEFERRABLE INITIALLY DEFERRED"
        } else {
            " DEFERRABLE"
        });
    }
    if !c.enabled {
        line.push_str(" DISABLE");
    } else if !c.validated {
        line.push_str(" ENABLE NOVALIDATE");
    }
    line
}

/// One index of a table, key parts in order (a column name, quoted, or
/// the expression of a function-based part).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Idx {
    pub owner: String,
    pub name: String,
    pub index_type: String,
    pub unique: bool,
    pub partitioned: bool,
    pub parts: Vec<String>,
}

/// `CREATE INDEX` for `idx` on `table_owner.table`, or a comment line for
/// index types this module does not write.
pub(crate) fn index_statement(idx: &Idx, table_owner: &str, table: &str) -> Option<String> {
    let t = idx.index_type.as_str();
    if t == "LOB" {
        // Created with its LOB column, never by hand.
        return None;
    }
    let base = t.trim_start_matches("FUNCTION-BASED ");
    let (kind, reverse) = match base {
        "NORMAL" => ("", false),
        "NORMAL/REV" => ("", true),
        "BITMAP" => ("BITMAP ", false),
        _ => {
            return Some(format!(
                "-- Index {} ({t}) is not shown here. Tick Show storage for the full script.",
                qn(&idx.owner, &idx.name)
            ))
        }
    };
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let mut sql = format!(
        "CREATE {unique}{kind}INDEX {} ON {} ({})",
        qn(&idx.owner, &idx.name),
        qn(table_owner, table),
        idx.parts.join(", ")
    );
    if reverse {
        sql.push_str(" REVERSE");
    }
    sql.push(';');
    if idx.partitioned {
        sql.push_str(" -- partitioned: tick Show storage for the partitions");
    }
    Some(sql)
}

/// Table facts that decide whether this module can write the table.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct TableInfo {
    pub temporary: bool,
    /// `SYS$SESSION` or `SYS$TRANSACTION` for a temporary table.
    pub duration: String,
    /// IOT, partitioned, nested, clustered or external: needs DBMS_METADATA.
    pub special: bool,
    pub comment: String,
}

/// The whole script of a table: CREATE TABLE with its constraints, then
/// its other indexes and comments.
pub(crate) fn table_script(
    owner: &str,
    table: &str,
    info: &TableInfo,
    cols: &[Col],
    cons: &[Cons],
    indexes: &[Idx],
) -> String {
    let mut lines: Vec<String> = cols
        .iter()
        .map(|col| {
            let check = cons
                .iter()
                .find(|c| c.is_not_null_check() && c.columns[0] == col.name);
            column_line(col, check)
        })
        .collect();
    for c in cons.iter().filter(|c| !c.is_not_null_check()) {
        lines.push(constraint_line(c, owner));
    }
    let temp = if info.temporary {
        "GLOBAL TEMPORARY "
    } else {
        ""
    };
    let mut out = format!(
        "CREATE {temp}TABLE {} (\n{}\n)",
        qn(owner, table),
        lines.join(",\n")
    );
    if info.temporary {
        out.push_str(if info.duration == "SYS$SESSION" {
            " ON COMMIT PRESERVE ROWS"
        } else {
            " ON COMMIT DELETE ROWS"
        });
    }
    out.push_str(";\n");
    let backing: Vec<(&str, &str)> = cons
        .iter()
        .filter(|c| matches!(c.kind.as_str(), "P" | "U") && !c.index_name.is_empty())
        .map(|c| (c.index_owner.as_str(), c.index_name.as_str()))
        .collect();
    let index_lines: Vec<String> = indexes
        .iter()
        .filter(|i| {
            !backing
                .iter()
                .any(|(o, n)| *n == i.name && (o.is_empty() || *o == i.owner))
        })
        .filter_map(|i| index_statement(i, owner, table))
        .collect();
    if !index_lines.is_empty() {
        out.push('\n');
        out.push_str(&index_lines.join("\n"));
        out.push('\n');
    }
    let mut comments = Vec::new();
    if !info.comment.is_empty() {
        comments.push(format!(
            "COMMENT ON TABLE {} IS {};",
            qn(owner, table),
            lit(&info.comment)
        ));
    }
    for col in cols.iter().filter(|c| !c.comment.is_empty()) {
        comments.push(format!(
            "COMMENT ON COLUMN {}.{} IS {};",
            qn(owner, table),
            q(&col.name),
            lit(&col.comment)
        ));
    }
    if !comments.is_empty() {
        out.push('\n');
        out.push_str(&comments.join("\n"));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn table_ddl(conn: &Conn, owner: &str, table: &str) -> Res<Option<String>> {
    // One trip for the table's facts, its indexes and their columns. Each
    // part is a plain filter on one dictionary view: joining ALL_INDEXES to
    // ALL_IND_COLUMNS (or ALL_CONSTRAINTS to itself) costs the server far
    // more than the rows are worth.
    let rows = text_rows(
        conn,
        "SELECT 'T', t.temporary, t.duration, t.iot_type, t.partitioned, t.nested, t.cluster_name, \
         (SELECT TO_CHAR(COUNT(*)) FROM all_external_tables e \
           WHERE e.owner = t.owner AND e.table_name = t.table_name), \
         (SELECT c.comments FROM all_tab_comments c \
           WHERE c.owner = t.owner AND c.table_name = t.table_name) \
         FROM all_tables t WHERE t.owner = :1 AND t.table_name = :2 \
         UNION ALL \
         SELECT 'I', i.owner, i.index_name, i.index_type, i.uniqueness, i.partitioned, \
         NULL, NULL, NULL FROM all_indexes i WHERE i.table_owner = :3 AND i.table_name = :4 \
         UNION ALL \
         SELECT 'C', c.index_owner, c.index_name, c.column_name, c.descend, \
         TO_CHAR(c.column_position), NULL, NULL, NULL \
         FROM all_ind_columns c WHERE c.table_owner = :5 AND c.table_name = :6",
        &[owner, table, owner, table, owner, table],
    )?;
    // No table row: an object table, a materialized view container or no
    // access.
    let Some(row) = rows.iter().find(|r| cell(r, 0) == "T") else {
        return Ok(None);
    };
    let info = TableInfo {
        temporary: cell(row, 1) == "Y",
        duration: cell(row, 2).to_string(),
        special: !cell(row, 3).is_empty()
            || cell(row, 4) == "YES"
            || cell(row, 5) == "YES"
            || !cell(row, 6).is_empty()
            || !matches!(cell(row, 7), "" | "0"),
        comment: cell(row, 8).to_string(),
    };
    if info.special {
        return Ok(None);
    }
    let expressions = if needs_expressions(&rows) {
        text_rows(
            conn,
            "SELECT index_owner, index_name, TO_CHAR(column_position), column_expression \
             FROM all_ind_expressions WHERE table_owner = :1 AND table_name = :2",
            &[owner, table],
        )?
    } else {
        Vec::new()
    };
    let indexes = indexes_from(&rows, &expressions);
    let col_rows = text_rows(
        conn,
        "SELECT c.column_name, c.data_type, c.data_type_owner, TO_CHAR(c.data_length), \
         TO_CHAR(c.char_length), c.char_used, TO_CHAR(c.data_precision), TO_CHAR(c.data_scale), \
         c.data_default, c.default_on_null, c.virtual_column, c.hidden_column, \
         c.identity_column, cc.comments \
         FROM all_tab_cols c \
         LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name \
           AND cc.column_name = c.column_name \
         WHERE c.owner = :1 AND c.table_name = :2 AND c.user_generated = 'YES' \
         ORDER BY c.column_id NULLS LAST, c.internal_column_id",
        &[owner, table],
    )?;
    let mut cols = Vec::with_capacity(col_rows.len());
    let mut has_identity = false;
    for r in &col_rows {
        // Vector shapes are not in these views.
        if cell(r, 1) == "VECTOR" {
            return Ok(None);
        }
        has_identity |= cell(r, 12) == "YES";
        cols.push(Col {
            name: cell(r, 0).to_string(),
            data_type: cell(r, 1).to_string(),
            type_owner: cell(r, 2).to_string(),
            data_length: cell(r, 3).to_string(),
            char_length: cell(r, 4).to_string(),
            char_used: cell(r, 5).to_string(),
            precision: cell(r, 6).to_string(),
            scale: cell(r, 7).to_string(),
            default: cell(r, 8).to_string(),
            default_on_null: cell(r, 9) == "YES",
            virtual_col: cell(r, 10) == "YES",
            invisible: cell(r, 11) == "YES",
            identity: String::new(),
            comment: cell(r, 13).to_string(),
        });
    }
    if cols.is_empty() {
        return Ok(None);
    }
    if has_identity {
        let rows = text_rows(
            conn,
            "SELECT column_name, generation_type, identity_options FROM all_tab_identity_cols \
             WHERE owner = :1 AND table_name = :2",
            &[owner, table],
        )?;
        for r in &rows {
            if let Some(col) = cols.iter_mut().find(|c| c.name == cell(r, 0)) {
                col.identity = identity_clause(cell(r, 1), col.default_on_null, cell(r, 2));
            }
        }
        // An identity column whose options could not be read.
        if cols
            .iter()
            .any(|c| c.default.contains("ISEQ$$") && c.identity.is_empty())
        {
            return Ok(None);
        }
    }
    let Some(cons) = table_constraints(conn, owner, table)? else {
        return Ok(None);
    };
    Ok(Some(table_script(
        owner, table, &info, &cols, &cons, &indexes,
    )))
}

/// The table's constraints with their columns, foreign keys with the
/// referenced table and columns. `None` when a foreign key points at a
/// constraint this user cannot see.
fn table_constraints(conn: &Conn, owner: &str, table: &str) -> Res<Option<Vec<Cons>>> {
    let rows = text_rows(
        conn,
        "SELECT c.constraint_name, c.constraint_type, c.generated, c.search_condition, \
         c.delete_rule, c.status, c.deferrable, c.deferred, c.validated, c.index_owner, \
         c.index_name, c.r_owner, c.r_constraint_name, cc.column_name \
         FROM all_constraints c \
         LEFT JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.table_name = c.table_name \
           AND cc.constraint_name = c.constraint_name \
         WHERE c.owner = :1 AND c.table_name = :2 AND c.constraint_type IN ('P', 'U', 'R', 'C') \
         ORDER BY DECODE(c.constraint_type, 'P', 1, 'U', 2, 'R', 3, 4), c.constraint_name, cc.position",
        &[owner, table],
    )?;
    let mut cons: Vec<Cons> = Vec::new();
    // Referenced (owner, constraint) of each foreign key, by index in `cons`.
    let mut refs: Vec<(usize, String, String)> = Vec::new();
    for r in &rows {
        let name = cell(r, 0);
        if cons.last().map(|c| c.name.as_str()) != Some(name) {
            cons.push(Cons {
                name: name.to_string(),
                kind: cell(r, 1).to_string(),
                generated_name: cell(r, 2) == "GENERATED NAME",
                condition: cell(r, 3).to_string(),
                delete_rule: cell(r, 4).to_string(),
                enabled: cell(r, 5) != "DISABLED",
                deferrable: cell(r, 6) == "DEFERRABLE",
                deferred: cell(r, 7) == "DEFERRED",
                validated: cell(r, 8) != "NOT VALIDATED",
                index_owner: cell(r, 9).to_string(),
                index_name: cell(r, 10).to_string(),
                ..Cons::default()
            });
            if cell(r, 1) == "R" {
                refs.push((
                    cons.len() - 1,
                    cell(r, 11).to_string(),
                    cell(r, 12).to_string(),
                ));
            }
        }
        if !cell(r, 13).is_empty() {
            let c = cons.last_mut().expect("pushed above");
            c.columns.push(cell(r, 13).to_string());
        }
    }
    if refs.is_empty() {
        return Ok(Some(cons));
    }
    let pairs: Vec<String> = (0..refs.len())
        .map(|i| format!("(:{}, :{})", 2 * i + 1, 2 * i + 2))
        .collect();
    let binds: Vec<&str> = refs
        .iter()
        .flat_map(|(_, o, n)| [o.as_str(), n.as_str()])
        .collect();
    let ref_rows = text_rows(
        conn,
        &format!(
            "SELECT owner, constraint_name, table_name, column_name FROM all_cons_columns \
             WHERE (owner, constraint_name) IN ({}) ORDER BY owner, constraint_name, position",
            pairs.join(", ")
        ),
        &binds,
    )?;
    for (ix, r_owner, r_name) in &refs {
        let c = &mut cons[*ix];
        for r in ref_rows
            .iter()
            .filter(|r| cell(r, 0) == r_owner && cell(r, 1) == r_name)
        {
            c.ref_owner = r_owner.clone();
            c.ref_table = cell(r, 2).to_string();
            c.ref_columns.push(cell(r, 3).to_string());
        }
        if c.ref_table.is_empty() {
            return Ok(None);
        }
    }
    Ok(Some(cons))
}

/// True when an `I` row of [`table_ddl`]'s query is a function-based index,
/// whose key expressions are in `ALL_IND_EXPRESSIONS`.
fn needs_expressions(rows: &[Vec<Option<String>>]) -> bool {
    rows.iter()
        .any(|r| cell(r, 0) == "I" && cell(r, 3).starts_with("FUNCTION-BASED"))
}

/// Indexes from the `I` (owner, name, type, uniqueness, partitioned) and
/// `C` (owner, name, column, descend, position) rows of an index query,
/// key parts in position order. `expressions` rows (owner, name, position,
/// expression) replace the hidden column of a function-based part.
pub(crate) fn indexes_from(
    rows: &[Vec<Option<String>>],
    expressions: &[Vec<Option<String>>],
) -> Vec<Idx> {
    let mut out: Vec<Idx> = rows
        .iter()
        .filter(|r| cell(r, 0) == "I")
        .map(|r| Idx {
            owner: cell(r, 1).to_string(),
            name: cell(r, 2).to_string(),
            index_type: cell(r, 3).to_string(),
            unique: cell(r, 4) == "UNIQUE",
            partitioned: cell(r, 5) == "YES",
            parts: Vec::new(),
        })
        .collect();
    out.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
    let mut parts: Vec<(&str, &str, u32, String)> = rows
        .iter()
        .filter(|r| cell(r, 0) == "C")
        .map(|r| {
            let (owner, name, position) = (cell(r, 1), cell(r, 2), cell(r, 5));
            let expression = expressions
                .iter()
                .find(|e| cell(e, 0) == owner && cell(e, 1) == name && cell(e, 2) == position)
                .map(|e| cell(e, 3).trim())
                .filter(|e| !e.is_empty());
            let mut part = match expression {
                Some(e) => e.to_string(),
                None => q(cell(r, 3)),
            };
            if cell(r, 4) == "DESC" {
                part.push_str(" DESC");
            }
            (owner, name, position.parse().unwrap_or(0), part)
        })
        .collect();
    parts.sort_by_key(|p| p.2);
    for (owner, name, _, part) in parts {
        if let Some(idx) = out.iter_mut().find(|i| i.owner == owner && i.name == name) {
            idx.parts.push(part);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Other objects
// ---------------------------------------------------------------------------

fn index_ddl(conn: &Conn, owner: &str, name: &str) -> Res<Option<String>> {
    let rows = text_rows(
        conn,
        "SELECT 'I', i.owner, i.index_name, i.index_type, i.uniqueness, i.partitioned, \
         i.table_owner, i.table_name, i.table_type FROM all_indexes i \
         WHERE i.owner = :1 AND i.index_name = :2 \
         UNION ALL \
         SELECT 'C', c.index_owner, c.index_name, c.column_name, c.descend, \
         TO_CHAR(c.column_position), NULL, NULL, NULL \
         FROM all_ind_columns c WHERE c.index_owner = :3 AND c.index_name = :4",
        &[owner, name, owner, name],
    )?;
    let Some(row) = rows.iter().find(|r| cell(r, 0) == "I") else {
        return Ok(None);
    };
    if cell(row, 8) != "TABLE" {
        return Ok(None);
    }
    let (table_owner, table) = (cell(row, 6).to_string(), cell(row, 7).to_string());
    let expressions = if needs_expressions(&rows) {
        text_rows(
            conn,
            "SELECT index_owner, index_name, TO_CHAR(column_position), column_expression \
             FROM all_ind_expressions WHERE index_owner = :1 AND index_name = :2",
            &[owner, name],
        )?
    } else {
        Vec::new()
    };
    Ok(indexes_from(&rows, &expressions)
        .first()
        .and_then(|idx| index_statement(idx, &table_owner, &table))
        .filter(|sql| !sql.starts_with("--")))
}

fn view_ddl(conn: &Conn, owner: &str, name: &str) -> Res<Option<String>> {
    let rows = text_rows(
        conn,
        "SELECT v.text, v.type_text, \
         (SELECT MAX(c.constraint_type) FROM all_constraints c \
           WHERE c.owner = v.owner AND c.table_name = v.view_name AND c.constraint_type IN ('V', 'O')) \
         FROM all_views v WHERE v.owner = :1 AND v.view_name = :2",
        &[owner, name],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    // An object view (OF type WITH OBJECT IDENTIFIER ...).
    if !cell(row, 1).is_empty() {
        return Ok(None);
    }
    let text = cell(row, 0).trim_end().to_string();
    if text.is_empty() {
        return Ok(None);
    }
    let columns = text_rows(
        conn,
        "SELECT column_name FROM all_tab_columns WHERE owner = :1 AND table_name = :2 \
         ORDER BY column_id",
        &[owner, name],
    )?;
    let names: Vec<String> = columns.iter().map(|r| q(cell(r, 0))).collect();
    Ok(Some(view_script(owner, name, &names, &text, cell(row, 2))))
}

/// CREATE VIEW from the stored query text. `option` is the view's `V`
/// (check option) or `O` (read only) constraint, if any.
pub(crate) fn view_script(
    owner: &str,
    name: &str,
    columns: &[String],
    text: &str,
    option: &str,
) -> String {
    let list = if columns.is_empty() {
        String::new()
    } else {
        format!(" ({})", columns.join(", "))
    };
    // Newer servers keep the clause in the stored text already.
    let upper = text.to_ascii_uppercase();
    let tail = match option {
        _ if upper.contains("WITH READ ONLY") || upper.contains("WITH CHECK OPTION") => "",
        "O" => "\nWITH READ ONLY",
        "V" => "\nWITH CHECK OPTION",
        _ => "",
    };
    format!(
        "CREATE OR REPLACE FORCE VIEW {}{list} AS\n{}{tail};",
        qn(owner, name),
        text.trim_end().trim_end_matches(';')
    )
}

fn sequence_ddl(conn: &Conn, owner: &str, name: &str) -> Res<Option<String>> {
    let rows = text_rows(
        conn,
        "SELECT TO_CHAR(min_value), TO_CHAR(max_value), TO_CHAR(increment_by), cycle_flag, \
         order_flag, TO_CHAR(cache_size), TO_CHAR(last_number) \
         FROM all_sequences WHERE sequence_owner = :1 AND sequence_name = :2",
        &[owner, name],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let cache = match cell(r, 5) {
        "" | "0" => "NOCACHE".to_string(),
        n => format!("CACHE {n}"),
    };
    Ok(Some(format!(
        "CREATE SEQUENCE {} MINVALUE {} MAXVALUE {} INCREMENT BY {} START WITH {} {cache} {} {};",
        qn(owner, name),
        cell(r, 0),
        cell(r, 1),
        cell(r, 2),
        cell(r, 6),
        if cell(r, 4) == "Y" {
            "ORDER"
        } else {
            "NOORDER"
        },
        if cell(r, 3) == "Y" {
            "CYCLE"
        } else {
            "NOCYCLE"
        },
    )))
}

fn synonym_ddl(conn: &Conn, owner: &str, name: &str) -> Res<Option<String>> {
    let rows = text_rows(
        conn,
        "SELECT table_owner, table_name, db_link FROM all_synonyms \
         WHERE owner = :1 AND synonym_name = :2",
        &[owner, name],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let target = match cell(r, 0) {
        "" => q(cell(r, 1)),
        target_owner => qn(target_owner, cell(r, 1)),
    };
    let link = match cell(r, 2) {
        "" => String::new(),
        link => format!("@{}", q(link)),
    };
    let synonym = if owner == "PUBLIC" {
        format!("PUBLIC SYNONYM {}", q(name))
    } else {
        format!("SYNONYM {}", qn(owner, name))
    };
    Ok(Some(format!(
        "CREATE OR REPLACE {synonym} FOR {target}{link};"
    )))
}

/// PL/SQL source of `types` (spec first, then body) from `ALL_SOURCE`.
fn source_units(conn: &Conn, owner: &str, name: &str, types: &[&str]) -> Res<Vec<String>> {
    let list = types.iter().map(|t| lit(t)).collect::<Vec<_>>().join(", ");
    let rows = text_rows(
        conn,
        &format!(
            "SELECT type, text FROM all_source WHERE owner = :1 AND name = :2 \
             AND type IN ({list}) ORDER BY DECODE(type, {}, 1, 2), line",
            lit(types[0])
        ),
        &[owner, name],
    )?;
    let mut units: Vec<(String, String)> = Vec::new();
    for r in &rows {
        let kind = cell(r, 0);
        if units.last().map(|u| u.0.as_str()) != Some(kind) {
            units.push((kind.to_string(), String::new()));
        }
        units
            .last_mut()
            .expect("pushed above")
            .1
            .push_str(cell(r, 1));
    }
    Ok(units
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| !text.trim().is_empty())
        .collect())
}

fn source_ddl(conn: &Conn, owner: &str, name: &str, types: &[&str]) -> Res<Option<String>> {
    let units = source_units(conn, owner, name, types)?;
    if units.is_empty() {
        return Ok(None);
    }
    Ok(Some(source_script(owner, name, &units)))
}

/// `CREATE OR REPLACE` + each unit, owner-qualified, `/` after each.
pub(crate) fn source_script(owner: &str, name: &str, units: &[String]) -> String {
    units
        .iter()
        .map(|unit| {
            format!(
                "CREATE OR REPLACE {}\n/",
                qualify_header(unit.trim_end(), owner, name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn trigger_ddl(conn: &Conn, owner: &str, name: &str) -> Res<Option<String>> {
    let units = source_units(conn, owner, name, &["TRIGGER"])?;
    if units.is_empty() {
        return Ok(None);
    }
    let mut out = source_script(owner, name, &units);
    let status = text_rows(
        conn,
        "SELECT status FROM all_triggers WHERE owner = :1 AND trigger_name = :2",
        &[owner, name],
    )?;
    if status.first().is_some_and(|r| cell(r, 0) == "DISABLED") {
        out.push_str(&format!("\n\nALTER TRIGGER {} DISABLE;", qn(owner, name)));
    }
    Ok(Some(out))
}

/// The next identifier of `rest` (upper-cased), moving `rest` past it.
fn take_word(rest: &mut &str) -> Option<String> {
    let trimmed = rest.trim_start();
    let end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
        .unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }
    let word = trimmed[..end].to_ascii_uppercase();
    *rest = &trimmed[end..];
    Some(word)
}

/// `PACKAGE pkg AS ...` as stored in `ALL_SOURCE` with the name written as
/// `"OWNER"."PKG"`, so the script recreates the unit in its own schema.
/// Text this does not recognize (a comment before the name, a name that
/// is already qualified) is returned unchanged.
pub(crate) fn qualify_header(source: &str, owner: &str, name: &str) -> String {
    let body = source.trim_start();
    let mut rest = body;
    let mut word = match take_word(&mut rest) {
        Some(w) => w,
        None => return body.to_string(),
    };
    if word == "EDITIONABLE" || word == "NONEDITIONABLE" {
        word = match take_word(&mut rest) {
            Some(w) => w,
            None => return body.to_string(),
        };
    }
    if !matches!(
        word.as_str(),
        "PACKAGE" | "TYPE" | "PROCEDURE" | "FUNCTION" | "TRIGGER"
    ) {
        return body.to_string();
    }
    if matches!(word.as_str(), "PACKAGE" | "TYPE") {
        let mut probe = rest;
        if take_word(&mut probe).as_deref() == Some("BODY") {
            rest = probe;
        }
    }
    // The name: quoted or bare, never followed by `.` (already qualified).
    let after_ws = rest.trim_start();
    let name_start = body.len() - after_ws.len();
    let (token_len, matches_name) = if let Some(inner) = after_ws.strip_prefix('"') {
        match inner.find('"') {
            Some(end) => (end + 2, &inner[..end] == name),
            None => return body.to_string(),
        }
    } else {
        let end = after_ws
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
            .unwrap_or(after_ws.len());
        (
            end,
            end > 0
                && after_ws[..end].eq_ignore_ascii_case(name)
                && name == name.to_ascii_uppercase(),
        )
    };
    if !matches_name || after_ws[token_len..].trim_start().starts_with('.') {
        return body.to_string();
    }
    format!(
        "{}{}{}",
        &body[..name_start],
        qn(owner, name),
        &after_ws[token_len..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, data_type: &str) -> Col {
        Col {
            name: name.into(),
            data_type: data_type.into(),
            ..Col::default()
        }
    }

    #[test]
    fn column_types_match_dbms_metadata_spelling() {
        let mut c = col("A", "NUMBER");
        assert_eq!(column_type(&c), "NUMBER");
        c.precision = "12".into();
        c.scale = "2".into();
        assert_eq!(column_type(&c), "NUMBER(12,2)");
        c.precision.clear();
        c.scale = "0".into();
        assert_eq!(column_type(&c), "NUMBER(*,0)");
        let mut v = col("B", "VARCHAR2");
        v.data_length = "400".into();
        v.char_length = "100".into();
        v.char_used = "C".into();
        assert_eq!(column_type(&v), "VARCHAR2(100 CHAR)");
        v.char_used = "B".into();
        assert_eq!(column_type(&v), "VARCHAR2(400)");
        let mut n = col("C", "NVARCHAR2");
        n.char_length = "30".into();
        assert_eq!(column_type(&n), "NVARCHAR2(30)");
        let mut r = col("D", "RAW");
        r.data_length = "16".into();
        assert_eq!(column_type(&r), "RAW(16)");
        assert_eq!(column_type(&col("E", "TIMESTAMP(6)")), "TIMESTAMP(6)");
        let mut obj = col("F", "ADDRESS_T");
        obj.type_owner = "HR".into();
        assert_eq!(column_type(&obj), "\"HR\".\"ADDRESS_T\"");
        let mut xml = col("G", "XMLTYPE");
        xml.type_owner = "SYS".into();
        assert_eq!(column_type(&xml), "XMLTYPE");
    }

    #[test]
    fn column_lines_carry_defaults_virtual_and_not_null() {
        let mut c = col("ID", "NUMBER");
        c.default = "0 \n".into();
        let mut check = cons("C", "SYS_C9", &["ID"]);
        check.generated_name = true;
        check.condition = "\"ID\" IS NOT NULL".into();
        assert_eq!(
            column_line(&c, Some(&check)),
            "  \"ID\" NUMBER DEFAULT 0 NOT NULL"
        );
        assert_eq!(column_line(&c, None), "  \"ID\" NUMBER DEFAULT 0");
        let mut named = check.clone();
        named.generated_name = false;
        named.name = "ID_NN".into();
        named.enabled = false;
        assert_eq!(
            column_line(&c, Some(&named)),
            "  \"ID\" NUMBER DEFAULT 0 CONSTRAINT \"ID_NN\" NOT NULL DISABLE"
        );
        c.default_on_null = true;
        assert_eq!(
            column_line(&c, Some(&check)),
            "  \"ID\" NUMBER DEFAULT ON NULL 0"
        );
        let mut v = col("TOTAL", "NUMBER");
        v.virtual_col = true;
        v.default = "\"A\"+\"B\"".into();
        assert_eq!(
            column_line(&v, None),
            "  \"TOTAL\" NUMBER GENERATED ALWAYS AS (\"A\"+\"B\") VIRTUAL"
        );
        let mut i = col("SECRET", "DATE");
        i.invisible = true;
        assert_eq!(column_line(&i, None), "  \"SECRET\" DATE INVISIBLE");
    }

    fn cons(kind: &str, name: &str, columns: &[&str]) -> Cons {
        Cons {
            name: name.into(),
            kind: kind.into(),
            enabled: true,
            validated: true,
            columns: columns.iter().map(|c| c.to_string()).collect(),
            ..Cons::default()
        }
    }

    #[test]
    fn table_script_writes_constraints_indexes_and_comments() {
        let id = col("ID", "NUMBER");
        let mut name = col("NAME", "VARCHAR2");
        name.data_length = "50".into();
        name.comment = "Customer's name".into();
        let mut dept = col("DEPT_ID", "NUMBER");
        dept.precision = "4".into();
        let mut pk = cons("P", "SYS_C001", &["ID"]);
        pk.generated_name = true;
        pk.index_owner = "HR".into();
        pk.index_name = "SYS_C001".into();
        let mut not_null = cons("C", "SYS_C002", &["ID"]);
        not_null.generated_name = true;
        not_null.condition = "\"ID\" IS NOT NULL".into();
        let mut fk = cons("R", "EMP_DEPT_FK", &["DEPT_ID"]);
        fk.ref_owner = "HR".into();
        fk.ref_table = "DEPARTMENTS".into();
        fk.ref_columns = vec!["DEPARTMENT_ID".into()];
        fk.delete_rule = "CASCADE".into();
        let mut check = cons("C", "NAME_CK", &["NAME"]);
        check.condition = "name <> 'x'".into();
        check.enabled = false;
        let backing = Idx {
            owner: "HR".into(),
            name: "SYS_C001".into(),
            index_type: "NORMAL".into(),
            unique: true,
            parts: vec!["\"ID\"".into()],
            ..Idx::default()
        };
        let by_name = Idx {
            owner: "HR".into(),
            name: "EMP_NAME_IX".into(),
            index_type: "FUNCTION-BASED NORMAL".into(),
            parts: vec!["UPPER(\"NAME\")".into(), "\"ID\" DESC".into()],
            ..Idx::default()
        };
        let lob = Idx {
            owner: "HR".into(),
            name: "SYS_IL0001".into(),
            index_type: "LOB".into(),
            ..Idx::default()
        };
        let info = TableInfo {
            comment: "People".into(),
            ..TableInfo::default()
        };
        let script = table_script(
            "HR",
            "EMP",
            &info,
            &[id, name, dept],
            &[pk, not_null, fk, check],
            &[backing, by_name, lob],
        );
        assert_eq!(
            script,
            "CREATE TABLE \"HR\".\"EMP\" (\n\
             \x20 \"ID\" NUMBER NOT NULL,\n\
             \x20 \"NAME\" VARCHAR2(50),\n\
             \x20 \"DEPT_ID\" NUMBER(4),\n\
             \x20 PRIMARY KEY (\"ID\"),\n\
             \x20 CONSTRAINT \"EMP_DEPT_FK\" FOREIGN KEY (\"DEPT_ID\") REFERENCES \"DEPARTMENTS\" (\"DEPARTMENT_ID\") ON DELETE CASCADE,\n\
             \x20 CONSTRAINT \"NAME_CK\" CHECK (name <> 'x') DISABLE\n\
             );\n\
             \n\
             CREATE INDEX \"HR\".\"EMP_NAME_IX\" ON \"HR\".\"EMP\" (UPPER(\"NAME\"), \"ID\" DESC);\n\
             \n\
             COMMENT ON TABLE \"HR\".\"EMP\" IS 'People';\n\
             COMMENT ON COLUMN \"HR\".\"EMP\".\"NAME\" IS 'Customer''s name';"
        );
    }

    #[test]
    fn identity_columns_keep_their_options() {
        let options = "START WITH: 1, INCREMENT BY: 1, MAX_VALUE: 9999999999999999999999999999, \
                       MIN_VALUE: 1, CYCLE_FLAG: N, CACHE_SIZE: 20, ORDER_FLAG: N, SCALE_FLAG: N";
        assert_eq!(
            identity_clause("BY DEFAULT", false, options),
            "GENERATED BY DEFAULT AS IDENTITY (START WITH 1 INCREMENT BY 1 MINVALUE 1 \
             MAXVALUE 9999999999999999999999999999 CACHE 20)"
        );
        assert_eq!(
            identity_clause("ALWAYS", false, "CACHE_SIZE: 0, CYCLE_FLAG: Y"),
            "GENERATED ALWAYS AS IDENTITY (NOCACHE CYCLE)"
        );
        let mut c = col("ID", "NUMBER");
        c.default = "\"HR\".\"ISEQ$$_1\".nextval".into();
        c.identity = identity_clause("BY DEFAULT", true, "");
        assert_eq!(
            column_line(&c, None),
            "  \"ID\" NUMBER GENERATED BY DEFAULT ON NULL AS IDENTITY"
        );
    }

    #[test]
    fn temporary_tables_keep_their_on_commit_rule() {
        let info = TableInfo {
            temporary: true,
            duration: "SYS$SESSION".into(),
            ..TableInfo::default()
        };
        let script = table_script("HR", "TMP", &info, &[col("A", "DATE")], &[], &[]);
        assert_eq!(
            script,
            "CREATE GLOBAL TEMPORARY TABLE \"HR\".\"TMP\" (\n  \"A\" DATE\n) ON COMMIT PRESERVE ROWS;"
        );
    }

    #[test]
    fn foreign_keys_to_other_schemas_are_qualified() {
        let mut fk = cons("R", "FK", &["A"]);
        fk.ref_owner = "OTHER".into();
        fk.ref_table = "T".into();
        fk.ref_columns = vec!["B".into()];
        fk.deferrable = true;
        fk.deferred = true;
        assert_eq!(
            constraint_line(&fk, "HR"),
            "  CONSTRAINT \"FK\" FOREIGN KEY (\"A\") REFERENCES \"OTHER\".\"T\" (\"B\") DEFERRABLE INITIALLY DEFERRED"
        );
    }

    #[test]
    fn unknown_index_types_become_a_comment() {
        let idx = Idx {
            owner: "HR".into(),
            name: "TXT".into(),
            index_type: "DOMAIN".into(),
            ..Idx::default()
        };
        assert!(index_statement(&idx, "HR", "T")
            .unwrap()
            .starts_with("-- Index"));
        let bitmap = Idx {
            owner: "HR".into(),
            name: "B".into(),
            index_type: "BITMAP".into(),
            parts: vec!["\"S\"".into()],
            ..Idx::default()
        };
        assert_eq!(
            index_statement(&bitmap, "HR", "T").unwrap(),
            "CREATE BITMAP INDEX \"HR\".\"B\" ON \"HR\".\"T\" (\"S\");"
        );
    }

    fn row(cells: &[&str]) -> Vec<Option<String>> {
        cells
            .iter()
            .map(|c| (!c.is_empty()).then(|| c.to_string()))
            .collect()
    }

    #[test]
    fn index_rows_assemble_in_key_order_with_expressions() {
        let rows = vec![
            row(&["T", "N", "", "", "NO", "NO", "", "0", ""]),
            row(&[
                "I",
                "HR",
                "B_IX",
                "FUNCTION-BASED NORMAL",
                "NONUNIQUE",
                "NO",
            ]),
            row(&["I", "HR", "A_IX", "NORMAL", "UNIQUE", "YES"]),
            row(&["C", "HR", "B_IX", "SAL", "DESC", "2"]),
            row(&["C", "HR", "B_IX", "SYS_NC00005$", "ASC", "1"]),
            row(&["C", "HR", "A_IX", "ID", "ASC", "1"]),
        ];
        assert!(needs_expressions(&rows));
        let exprs = vec![row(&["HR", "B_IX", "1", "UPPER(\"NAME\") "])];
        let idx = indexes_from(&rows, &exprs);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].name, "A_IX");
        assert!(idx[0].unique && idx[0].partitioned);
        assert_eq!(idx[0].parts, vec!["\"ID\"".to_string()]);
        assert_eq!(
            idx[1].parts,
            vec!["UPPER(\"NAME\")".to_string(), "\"SAL\" DESC".to_string()]
        );
    }

    #[test]
    fn views_get_their_column_list_and_options() {
        assert_eq!(
            view_script(
                "HR",
                "V",
                &["\"ID\"".into(), "\"N\"".into()],
                "SELECT id, n FROM t\n",
                "O"
            ),
            "CREATE OR REPLACE FORCE VIEW \"HR\".\"V\" (\"ID\", \"N\") AS\nSELECT id, n FROM t\nWITH READ ONLY;"
        );
        assert_eq!(
            view_script("HR", "V", &[], "SELECT 1 x FROM t WITH READ ONLY", "O"),
            "CREATE OR REPLACE FORCE VIEW \"HR\".\"V\" AS\nSELECT 1 x FROM t WITH READ ONLY;"
        );
    }

    #[test]
    fn source_headers_get_the_owner() {
        assert_eq!(
            qualify_header("PACKAGE pkg AS x NUMBER; END;", "HR", "PKG"),
            "PACKAGE \"HR\".\"PKG\" AS x NUMBER; END;"
        );
        assert_eq!(
            qualify_header("package body Pkg is\nend;", "HR", "PKG"),
            "package body \"HR\".\"PKG\" is\nend;"
        );
        assert_eq!(
            qualify_header("TYPE \"Point\" AS OBJECT (x NUMBER);", "HR", "Point"),
            "TYPE \"HR\".\"Point\" AS OBJECT (x NUMBER);"
        );
        assert_eq!(
            qualify_header("EDITIONABLE TRIGGER trg BEFORE INSERT ON t", "HR", "TRG"),
            "EDITIONABLE TRIGGER \"HR\".\"TRG\" BEFORE INSERT ON t"
        );
        // Already qualified, or not recognized: unchanged.
        assert_eq!(
            qualify_header("PROCEDURE hr.p IS BEGIN NULL; END;", "HR", "P"),
            "PROCEDURE hr.p IS BEGIN NULL; END;"
        );
        assert_eq!(
            qualify_header("/* c */ PROCEDURE p", "HR", "P"),
            "/* c */ PROCEDURE p"
        );
        assert_eq!(
            source_script("HR", "P", &["PROCEDURE p IS BEGIN NULL; END;\n".into()]),
            "CREATE OR REPLACE PROCEDURE \"HR\".\"P\" IS BEGIN NULL; END;\n/"
        );
    }
}
