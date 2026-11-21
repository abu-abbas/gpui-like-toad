//! The transfer runner. It talks to both databases through [`TransferIo`],
//! so the same code runs against the app's connection workers, an SQL
//! script file and in-memory test databases.
//!
//! Safety: the runner never logs statements or row values, only table
//! names, counts and the database's own error text. Stop is checked
//! between batches; an unfinished table is rolled back when it runs in a
//! transaction.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::client::{DbClient, DbError};
use crate::dialect::DbKind;
use crate::fetch::{FetchLimits, EXPORT_MAX_CELL_BYTES};
use crate::model::{ColumnDef, ConstraintDef, IndexDef, ObjectKind, QueryResult};

use super::spec::{Fields, Rows, TransferSpec};
use super::sqlgen::{self, ColumnPlan, Resume};

/// What the runner needs from one side of a transfer.
pub trait TransferIo {
    fn kind(&self) -> DbKind;
    /// The target is a script that only records statements: nothing can be
    /// read back, so existing tables are assumed to match.
    fn is_script(&self) -> bool {
        false
    }
    fn describe(&mut self, schema: &str, table: &str) -> Result<Vec<ColumnDef>, DbError>;
    fn indexes(&mut self, schema: &str, table: &str) -> Result<Vec<IndexDef>, DbError>;
    /// Constraints of a table; a side that cannot tell reports none.
    fn constraints(&mut self, _schema: &str, _table: &str) -> Result<Vec<ConstraintDef>, DbError> {
        Ok(Vec::new())
    }
    /// `CREATE TRIGGER` scripts of a table; a side that cannot tell reports none.
    fn triggers(&mut self, _schema: &str, _table: &str) -> Result<Vec<String>, DbError> {
        Ok(Vec::new())
    }
    fn table_exists(&mut self, schema: &str, table: &str) -> Result<bool, DbError>;
    /// Run a SELECT, at most `max_rows` rows, cells up to the export limit.
    fn read(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
    /// Run statements in order. On failure returns how many succeeded
    /// before it, so the caller can skip exactly the failing statement.
    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)>;
    fn commit(&mut self) -> Result<(), DbError>;
    fn rollback(&mut self) -> Result<(), DbError>;
    /// The target takes rows directly (CSV files) instead of SQL: no
    /// tables, constraints or indexes are created, and `write_rows` is used.
    fn writes_rows(&self) -> bool {
        false
    }
    /// Append `rows` (values in `columns` order) to the target table's file.
    fn write_rows(
        &mut self,
        _table: &str,
        _columns: &[String],
        _rows: &[Vec<crate::model::Value>],
    ) -> Result<(), DbError> {
        Err(DbError::new("this target does not take rows"))
    }
}

/// [`TransferIo`] over a plain [`DbClient`] (tests, and the app's workers
/// run the same calls through jobs).
pub struct ClientIo<'a>(pub &'a mut dyn DbClient);

impl TransferIo for ClientIo<'_> {
    fn kind(&self) -> DbKind {
        self.0.kind()
    }
    fn describe(&mut self, schema: &str, table: &str) -> Result<Vec<ColumnDef>, DbError> {
        self.0.describe_table(schema, table)
    }
    fn indexes(&mut self, schema: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        self.0.list_indexes(schema, table)
    }
    fn constraints(&mut self, schema: &str, table: &str) -> Result<Vec<ConstraintDef>, DbError> {
        self.0.list_constraints(schema, table)
    }
    fn triggers(&mut self, schema: &str, table: &str) -> Result<Vec<String>, DbError> {
        self.0.trigger_scripts(schema, table)
    }
    fn table_exists(&mut self, schema: &str, table: &str) -> Result<bool, DbError> {
        Ok(self
            .0
            .list_objects(schema)?
            .iter()
            .any(|o| o.kind == ObjectKind::Table && o.name == table))
    }
    fn read(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        self.0.query(sql, read_limits(max_rows))
    }
    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)> {
        let mut total = 0;
        for (ix, sql) in statements.iter().enumerate() {
            total += self.0.execute(sql).map_err(|e| (ix, e))?;
        }
        Ok(total)
    }
    fn commit(&mut self) -> Result<(), DbError> {
        self.0.commit()
    }
    fn rollback(&mut self) -> Result<(), DbError> {
        self.0.rollback()
    }
}

/// Limits of one source page: a whole batch of rows, long cells allowed
/// (LOBs are read), bounded in bytes so a batch of huge rows stops early.
pub fn read_limits(max_rows: usize) -> FetchLimits {
    FetchLimits {
        max_rows,
        max_cell_bytes: EXPORT_MAX_CELL_BYTES,
        max_result_bytes: 64 * 1024 * 1024,
        read_lobs: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOutcome {
    pub source: String,
    pub target: String,
    pub rows: u64,
    /// Rows left out by Continue on error.
    pub skipped: u64,
    pub created: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Log(LogLevel, String),
    TableStart {
        index: usize,
        total: usize,
        source: String,
    },
    /// Rows copied so far in the current table.
    Rows {
        index: usize,
        done: u64,
    },
    TableEnd {
        index: usize,
        outcome: TableOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferSummary {
    pub tables: Vec<TableOutcome>,
    pub cancelled: bool,
    /// The spec was refused before anything ran.
    pub invalid: Option<String>,
}

impl TransferSummary {
    pub fn failed_tables(&self) -> usize {
        self.tables.iter().filter(|t| t.error.is_some()).count()
    }
    pub fn rows(&self) -> u64 {
        self.tables.iter().map(|t| t.rows).sum()
    }
    pub fn ok(&self) -> bool {
        self.invalid.is_none() && !self.cancelled && self.failed_tables() == 0
    }
}

struct Ctx<'a> {
    cancel: &'a AtomicBool,
    emit: &'a mut dyn FnMut(Event),
    /// Source table -> target name, for the tables that keep all their
    /// columns (foreign keys may point at those).
    targets: std::collections::HashMap<String, String>,
    /// `ALTER TABLE` statements to run once every table is in.
    deferred: Vec<String>,
}

impl Ctx<'_> {
    fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        (self.emit)(Event::Log(level, text.into()));
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Run the transfer. Always returns; failures are in the summary.
pub fn run_transfer(
    src: &mut dyn TransferIo,
    dst: &mut dyn TransferIo,
    spec: &TransferSpec,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> TransferSummary {
    let targets = spec
        .tables
        .iter()
        .filter(|t| matches!(t.fields, Fields::All))
        .map(|t| {
            (
                t.source.clone(),
                sqlgen::convert_name(t.target_name(), spec.options.convert_case),
            )
        })
        .collect();
    let mut cx = Ctx {
        cancel,
        emit,
        targets,
        deferred: Vec::new(),
    };
    let mut summary = TransferSummary::default();
    if let Err(e) = spec.validate() {
        cx.log(LogLevel::Error, e.clone());
        summary.invalid = Some(e);
        return summary;
    }
    let total = spec.tables.len();
    let locked = spec.options.lock_source && lock_sources(src, spec, &mut cx);
    for (index, table) in spec.tables.iter().enumerate() {
        if cx.cancelled() {
            summary.cancelled = true;
            break;
        }
        (cx.emit)(Event::TableStart {
            index,
            total,
            source: table.source.clone(),
        });
        let outcome = transfer_table(src, dst, spec, index, table, &mut cx);
        if let Some(err) = &outcome.error {
            cx.log(LogLevel::Error, format!("{}: {err}", outcome.source));
        } else {
            cx.log(
                LogLevel::Info,
                format!("{}: {} rows copied.", outcome.source, outcome.rows),
            );
        }
        let failed = outcome.error.is_some();
        (cx.emit)(Event::TableEnd {
            index,
            outcome: outcome.clone(),
        });
        summary.tables.push(outcome);
        if failed && !spec.options.continue_on_error {
            cx.log(LogLevel::Warn, "Stopped after the first error.");
            break;
        }
    }
    if !cx.deferred.is_empty() && !cx.cancelled() {
        for sql in std::mem::take(&mut cx.deferred) {
            if let Err((_, e)) = dst.execute_many(&[sql]) {
                cx.log(LogLevel::Warn, format!("foreign key not created: {e}"));
            }
        }
        if let Err(e) = dst.commit() {
            cx.log(LogLevel::Warn, format!("foreign keys not committed: {e}"));
        }
    }
    if locked {
        // The locks end with the source's transaction.
        let _ = src.rollback();
    }
    if cx.cancelled() {
        summary.cancelled = true;
        cx.log(LogLevel::Warn, "Stopped by the user.");
    }
    summary
}

/// `LOCK TABLE ... IN SHARE MODE` on every source table (Oracle): others
/// can read but not change them until the transfer ends. Returns whether
/// any lock is held.
fn lock_sources(src: &mut dyn TransferIo, spec: &TransferSpec, cx: &mut Ctx<'_>) -> bool {
    if src.kind() != DbKind::Oracle {
        cx.log(
            LogLevel::Info,
            "Lock source tables: SQLite locks the file itself while it is written; nothing to lock.",
        );
        return false;
    }
    let d = src.kind().dialect();
    let mut any = false;
    for t in &spec.tables {
        let sql = format!(
            "LOCK TABLE {} IN SHARE MODE",
            d.table_ref(&spec.source_schema, &t.source)
        );
        match src.execute_many(&[sql]) {
            Ok(_) => any = true,
            Err((_, e)) => cx.log(
                LogLevel::Warn,
                format!("{}: source table not locked: {e}", t.source),
            ),
        }
    }
    any
}

fn db(e: DbError) -> String {
    e.to_string()
}

fn transfer_table(
    src: &mut dyn TransferIo,
    dst: &mut dyn TransferIo,
    spec: &TransferSpec,
    index: usize,
    table: &super::spec::TableSpec,
    cx: &mut Ctx<'_>,
) -> TableOutcome {
    let target_name = sqlgen::convert_name(table.target_name(), spec.options.convert_case);
    let mut outcome = TableOutcome {
        source: table.source.clone(),
        target: target_name.clone(),
        rows: 0,
        skipped: 0,
        created: false,
        error: None,
    };
    if let Err(e) = copy_table(src, dst, spec, index, table, &target_name, &mut outcome, cx) {
        outcome.error = Some(e);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn copy_table(
    src: &mut dyn TransferIo,
    dst: &mut dyn TransferIo,
    spec: &TransferSpec,
    index: usize,
    table: &super::spec::TableSpec,
    target_name: &str,
    outcome: &mut TableOutcome,
    cx: &mut Ctx<'_>,
) -> Result<(), String> {
    let o = &spec.options;
    let (sk, dk) = (src.kind(), dst.kind());
    let dd = dk.dialect();
    let source_columns = src
        .describe(&spec.source_schema, &table.source)
        .map_err(db)?;
    if source_columns.is_empty() {
        return Err("table not found in the source schema".to_string());
    }
    let mut plan = sqlgen::plan_columns(sk, dk, &source_columns, &table.fields, o.convert_case)?;
    if !o.include_defaults {
        for p in &mut plan {
            p.default = None;
        }
    }
    let mut key_columns: Vec<Vec<String>> = Vec::new();

    if dst.writes_rows() {
        // CSV: one file per table; structure options do not apply.
        outcome.created = true;
        if o.create_records && !cx.cancelled() {
            copy_rows(
                src,
                dst,
                spec,
                index,
                table,
                target_name,
                &plan,
                outcome,
                cx,
            )?;
        }
        // A table without rows still gets its header-only file.
        dst.write_rows(
            target_name,
            &plan.iter().map(|p| p.target.clone()).collect::<Vec<_>>(),
            &[],
        )
        .map_err(db)?;
        return Ok(());
    }

    let mut exists = if dst.is_script() {
        !o.create_tables
    } else {
        dst.table_exists(&spec.target_schema, target_name)
            .map_err(db)?
    };
    if exists && o.create_tables && o.drop_target {
        let sql = crate::ddl::drop_table_sql(dd, &spec.target_schema, target_name);
        dst.execute_many(&[sql]).map_err(|(_, e)| db(e))?;
        cx.log(
            LogLevel::Warn,
            format!("{target_name}: existing target table dropped."),
        );
        exists = false;
    }
    if exists {
        if !dst.is_script() {
            adopt_target_columns(dst, &spec.target_schema, target_name, &mut plan)?;
        }
        if o.create_records && o.empty_target {
            let sql = crate::ddl::empty_table_sql(dd, &spec.target_schema, target_name);
            dst.execute_many(&[sql]).map_err(|(_, e)| db(e))?;
            dst.commit().map_err(db)?;
            cx.log(
                LogLevel::Warn,
                format!("{target_name}: existing rows deleted."),
            );
        }
    } else {
        if !o.create_tables {
            return Err("the target table does not exist and Create tables is off".to_string());
        }
        let cplan = plan_table_constraints(src, dk, spec, table, &plan, cx);
        let sql = sqlgen::create_table_sql_with(
            dd,
            &spec.target_schema,
            target_name,
            &plan,
            &cplan.inline,
        )?;
        dst.execute_many(&[sql]).map_err(|(_, e)| db(e))?;
        for clause in &cplan.after {
            cx.deferred.push(format!(
                "ALTER TABLE {} ADD {clause}",
                dd.table_ref(&spec.target_schema, target_name)
            ));
        }
        key_columns = cplan.key_columns;
        outcome.created = true;
        cx.log(LogLevel::Info, format!("{target_name}: table created."));
    }

    if o.create_records && !cx.cancelled() {
        copy_rows(
            src,
            dst,
            spec,
            index,
            table,
            target_name,
            &plan,
            outcome,
            cx,
        )?;
    }

    if outcome.created && o.create_tables && o.include_indexes && !cx.cancelled() {
        match src.indexes(&spec.source_schema, &table.source) {
            Err(e) => cx.log(
                LogLevel::Warn,
                format!("{target_name}: indexes not read: {e}"),
            ),
            Ok(indexes) => {
                for idx in &indexes {
                    // Already backed by a copied primary key / unique constraint.
                    if key_columns.contains(&idx.columns) {
                        continue;
                    }
                    let Some(sql) = sqlgen::create_index_sql(
                        dd,
                        &spec.target_schema,
                        target_name,
                        idx,
                        &plan,
                        o.convert_case,
                    ) else {
                        cx.log(
                            LogLevel::Warn,
                            format!(
                                "{target_name}: index {} not copied (unsupported kind or column).",
                                idx.name
                            ),
                        );
                        continue;
                    };
                    if let Err((_, e)) = dst.execute_many(&[sql]) {
                        cx.log(
                            LogLevel::Warn,
                            format!("{target_name}: index {} not created: {e}", idx.name),
                        );
                    }
                }
            }
        }
    }
    if outcome.created && o.include_triggers && !cx.cancelled() {
        copy_triggers(src, dst, spec, table, target_name, cx);
    }
    // DDL after the last commit (indexes, or a table created without
    // rows) must not be left in an open transaction.
    dst.commit().map_err(db)?;
    Ok(())
}

/// Re-create the source table's triggers on the new target table. Same
/// engine only: trigger bodies are engine specific. The text is the
/// database's own `CREATE TRIGGER` script; Oracle's schema prefix is
/// rewritten to the target schema. A trigger that fails is logged and
/// skipped, the table stays.
fn copy_triggers(
    src: &mut dyn TransferIo,
    dst: &mut dyn TransferIo,
    spec: &TransferSpec,
    table: &super::spec::TableSpec,
    target_name: &str,
    cx: &mut Ctx<'_>,
) {
    let name = &table.source;
    if src.kind() != dst.kind() {
        cx.log(
            LogLevel::Warn,
            format!("{name}: triggers not copied: trigger bodies are engine specific."),
        );
        return;
    }
    if !matches!(table.fields, Fields::All) || !target_name.eq_ignore_ascii_case(name) {
        cx.log(
            LogLevel::Warn,
            format!(
                "{name}: triggers not copied: the table or its columns were renamed or left out."
            ),
        );
        return;
    }
    let scripts = match src.triggers(&spec.source_schema, name) {
        Ok(s) => s,
        Err(e) => {
            cx.log(LogLevel::Warn, format!("{name}: triggers not read: {e}"));
            return;
        }
    };
    for script in scripts {
        let sql = if src.kind() == DbKind::Oracle {
            script.replace(
                &format!("\"{}\".", spec.source_schema),
                &format!("\"{}\".", spec.target_schema),
            )
        } else {
            script
        };
        if let Err((_, e)) = dst.execute_many(&[sql]) {
            cx.log(
                LogLevel::Warn,
                format!("{target_name}: a trigger was not created: {e}"),
            );
        }
    }
}

/// Read the source table's constraints and decide what the new target
/// table gets. Problems are logged, never fatal.
fn plan_table_constraints(
    src: &mut dyn TransferIo,
    dst_kind: DbKind,
    spec: &TransferSpec,
    table: &super::spec::TableSpec,
    plan: &[ColumnPlan],
    cx: &mut Ctx<'_>,
) -> sqlgen::ConstraintPlan {
    let o = &spec.options;
    if !(o.include_primary_keys || o.include_uniques || o.include_checks || o.include_foreign_keys)
    {
        return Default::default();
    }
    let constraints = match src.constraints(&spec.source_schema, &table.source) {
        Ok(c) => c,
        Err(e) => {
            cx.log(
                LogLevel::Warn,
                format!("{}: constraints not read: {e}", table.source),
            );
            return Default::default();
        }
    };
    let cplan = sqlgen::plan_constraints(
        src.kind(),
        dst_kind.dialect(),
        &spec.target_schema,
        &constraints,
        plan,
        matches!(table.fields, Fields::All),
        o,
        &|r| cx.targets.get(r).cloned(),
    );
    for line in &cplan.skipped {
        cx.log(LogLevel::Warn, format!("{}: {line}", table.source));
    }
    cplan
}

/// An existing target keeps its own column types and spelling.
fn adopt_target_columns(
    dst: &mut dyn TransferIo,
    schema: &str,
    table: &str,
    plan: &mut [ColumnPlan],
) -> Result<(), String> {
    let existing = dst.describe(schema, table).map_err(db)?;
    for p in plan.iter_mut() {
        let found = existing
            .iter()
            .find(|c| c.name == p.target)
            .or_else(|| {
                existing
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&p.target))
            })
            .ok_or_else(|| format!("the existing target table has no column {}", p.target))?;
        p.target = found.name.clone();
        p.target_type = found.data_type.clone();
    }
    Ok(())
}

enum Paging {
    Keyset(Resume),
    Offset(usize),
}

#[allow(clippy::too_many_arguments)]
fn copy_rows(
    src: &mut dyn TransferIo,
    dst: &mut dyn TransferIo,
    spec: &TransferSpec,
    index: usize,
    table: &super::spec::TableSpec,
    target_name: &str,
    plan: &[ColumnPlan],
    outcome: &mut TableOutcome,
    cx: &mut Ctx<'_>,
) -> Result<(), String> {
    let o = &spec.options;
    let (sd, dd) = (src.kind().dialect(), dst.kind().dialect());
    let batch = table.rows.batch();
    let with_list = o.complete_inserts || !matches!(table.fields, Fields::All) || !outcome.created;
    let conditions: Vec<Option<&str>> = match &table.rows {
        Rows::All { .. } => vec![None],
        Rows::Custom { conditions, .. } => conditions.iter().map(|c| Some(c.as_str())).collect(),
    };
    let result = (|| -> Result<(), String> {
        'sets: for (set_ix, cond) in conditions.iter().enumerate() {
            if conditions.len() > 1 {
                cx.log(
                    LogLevel::Info,
                    format!(
                        "{}: recordset {} of {}.",
                        table.source,
                        set_ix + 1,
                        conditions.len()
                    ),
                );
            }
            let mut paging = Paging::Keyset(Resume::Start);
            let mut first = true;
            loop {
                if cx.cancelled() {
                    break 'sets;
                }
                let (sql, keyed) = match &paging {
                    Paging::Keyset(resume) => (
                        sqlgen::keyset_select_sql(
                            sd,
                            &spec.source_schema,
                            &table.source,
                            plan,
                            *cond,
                            resume,
                            batch,
                        )?,
                        true,
                    ),
                    Paging::Offset(n) => (
                        sqlgen::offset_select_sql(
                            sd,
                            &spec.source_schema,
                            &table.source,
                            plan,
                            *cond,
                            *n,
                            batch,
                        ),
                        false,
                    ),
                };
                let page = match src.read(&sql, batch) {
                    Ok(p) => p,
                    Err(e) if first && keyed => {
                        cx.log(
                            LogLevel::Warn,
                            format!(
                                "{}: no usable row key ({e}); paging by offset, row order is not guaranteed.",
                                table.source
                            ),
                        );
                        paging = Paging::Offset(0);
                        first = false;
                        continue;
                    }
                    Err(e) => return Err(format!("reading the source failed: {e}")),
                };
                first = false;
                if page.rows.is_empty() {
                    break;
                }
                let more = page.truncated || page.rows.len() >= batch;
                if dst.writes_rows() {
                    let names: Vec<String> = plan.iter().map(|p| p.target.clone()).collect();
                    let skip = usize::from(keyed);
                    let rows: Vec<Vec<crate::model::Value>> =
                        page.rows.iter().map(|r| r[skip..].to_vec()).collect();
                    dst.write_rows(target_name, &names, &rows).map_err(db)?;
                    outcome.rows += rows.len() as u64;
                    (cx.emit)(Event::Rows {
                        index,
                        done: outcome.rows,
                    });
                    if !more {
                        break;
                    }
                    paging = match paging {
                        Paging::Keyset(_) => match page.rows.last().and_then(|r| r.first()) {
                            Some(key) => Paging::Keyset(Resume::After(key.clone())),
                            None => break,
                        },
                        Paging::Offset(n) => Paging::Offset(n + page.rows.len()),
                    };
                    continue;
                }
                let statements = sqlgen::insert_statements(
                    dd,
                    &spec.target_schema,
                    target_name,
                    plan,
                    &page.rows,
                    keyed,
                    with_list,
                );
                let mut good = Vec::with_capacity(statements.len());
                for st in statements {
                    match st {
                        Ok(sql) => good.push(sql),
                        Err(e) if o.continue_on_error => {
                            outcome.skipped += 1;
                            if outcome.skipped <= 20 {
                                cx.log(LogLevel::Warn, format!("{}: skipped {e}", table.source));
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                run_batch(dst, &good, o.continue_on_error, outcome, &table.source, cx)?;
                (cx.emit)(Event::Rows {
                    index,
                    done: outcome.rows,
                });
                if !o.use_transaction {
                    dst.commit().map_err(db)?;
                }
                if !more {
                    break;
                }
                paging = match paging {
                    Paging::Keyset(_) => match page.rows.last().and_then(|r| r.first()) {
                        Some(key) => Paging::Keyset(Resume::After(key.clone())),
                        None => break,
                    },
                    Paging::Offset(n) => Paging::Offset(n + page.rows.len()),
                };
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) if cx.cancelled() => {
            if o.use_transaction {
                let _ = dst.rollback();
                outcome.rows = 0;
                cx.log(
                    LogLevel::Warn,
                    format!("{target_name}: stopped, rows of this table rolled back."),
                );
            }
            Ok(())
        }
        Ok(()) => {
            if o.use_transaction {
                dst.commit().map_err(db)?;
            }
            Ok(())
        }
        Err(e) => {
            if o.use_transaction {
                let _ = dst.rollback();
                outcome.rows = 0;
                cx.log(
                    LogLevel::Warn,
                    format!("{target_name}: rows of this table rolled back."),
                );
            }
            Err(e)
        }
    }
}

/// Run one batch of INSERTs. With Continue on error a failing statement is
/// skipped (counted, the first few logged) and the rest still run.
fn run_batch(
    dst: &mut dyn TransferIo,
    statements: &[String],
    continue_on_error: bool,
    outcome: &mut TableOutcome,
    table: &str,
    cx: &mut Ctx<'_>,
) -> Result<(), String> {
    let mut rest = statements;
    while !rest.is_empty() {
        match dst.execute_many(rest) {
            Ok(_) => {
                outcome.rows += rest.len() as u64;
                return Ok(());
            }
            Err((done, e)) => {
                outcome.rows += done as u64;
                if !continue_on_error {
                    return Err(format!(
                        "writing the target failed after {} rows: {e}",
                        outcome.rows
                    ));
                }
                outcome.skipped += 1;
                if outcome.skipped <= 20 {
                    cx.log(LogLevel::Warn, format!("{table}: row skipped: {e}"));
                }
                rest = &rest[done + 1..];
            }
        }
    }
    Ok(())
}

/// A target that writes an SQL script instead of touching a database:
/// `statement;` per line, `COMMIT;` where the runner commits. The script's
/// dialect is `kind`, so it can be run later on that kind of database.
pub struct ScriptIo<W: std::io::Write> {
    pub kind: DbKind,
    out: W,
}

impl<W: std::io::Write> ScriptIo<W> {
    pub fn new(kind: DbKind, out: W) -> Self {
        ScriptIo { kind, out }
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    fn line(&mut self, text: &str) -> Result<(), DbError> {
        writeln!(self.out, "{text}")
            .map_err(|e| DbError::new(format!("writing the script failed: {e}")))
    }
}

impl<W: std::io::Write> TransferIo for ScriptIo<W> {
    fn kind(&self) -> DbKind {
        self.kind
    }
    fn is_script(&self) -> bool {
        true
    }
    fn describe(&mut self, _: &str, _: &str) -> Result<Vec<ColumnDef>, DbError> {
        Ok(Vec::new())
    }
    fn indexes(&mut self, _: &str, _: &str) -> Result<Vec<IndexDef>, DbError> {
        Ok(Vec::new())
    }
    fn table_exists(&mut self, _: &str, _: &str) -> Result<bool, DbError> {
        Ok(false)
    }
    fn read(&mut self, _: &str, _: usize) -> Result<QueryResult, DbError> {
        Err(DbError::new("an SQL script cannot be read"))
    }
    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)> {
        for (ix, sql) in statements.iter().enumerate() {
            self.line(&format!("{sql};")).map_err(|e| (ix, e))?;
        }
        Ok(statements.len() as u64)
    }
    fn commit(&mut self) -> Result<(), DbError> {
        self.line("COMMIT;")
    }
    fn rollback(&mut self) -> Result<(), DbError> {
        self.line("ROLLBACK;")
    }
}

/// Writes each table as `<folder>/<table>.csv` (header row, formula-safe
/// text cells). Nothing is read back; structure statements are ignored.
pub struct CsvIo {
    dir: std::path::PathBuf,
    overwrite: bool,
    open: std::collections::HashMap<String, std::io::BufWriter<std::fs::File>>,
}

impl CsvIo {
    /// `overwrite`: replace files that already exist in the folder.
    pub fn new(dir: impl Into<std::path::PathBuf>, overwrite: bool) -> Self {
        CsvIo {
            dir: dir.into(),
            overwrite,
            open: Default::default(),
        }
    }

    /// File name for a table: keeps letters, digits and `_-.`, so a table
    /// name can never point outside the folder.
    pub fn file_name(table: &str) -> String {
        let mut name: String = table
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if name.is_empty() {
            name.push('_');
        }
        format!("{name}.csv")
    }

    /// Flush and close every file.
    pub fn finish(&mut self) -> Result<(), DbError> {
        use std::io::Write;
        for (_, mut w) in self.open.drain() {
            w.flush()
                .map_err(|e| DbError::new(format!("writing a CSV file failed: {e}")))?;
        }
        Ok(())
    }
}

impl TransferIo for CsvIo {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }
    fn is_script(&self) -> bool {
        true
    }
    fn writes_rows(&self) -> bool {
        true
    }
    fn describe(&mut self, _: &str, _: &str) -> Result<Vec<ColumnDef>, DbError> {
        Ok(Vec::new())
    }
    fn indexes(&mut self, _: &str, _: &str) -> Result<Vec<IndexDef>, DbError> {
        Ok(Vec::new())
    }
    fn table_exists(&mut self, _: &str, _: &str) -> Result<bool, DbError> {
        Ok(false)
    }
    fn read(&mut self, _: &str, _: usize) -> Result<QueryResult, DbError> {
        Err(DbError::new("a CSV target cannot be read"))
    }
    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)> {
        Ok(statements.len() as u64)
    }
    fn commit(&mut self) -> Result<(), DbError> {
        Ok(())
    }
    fn rollback(&mut self) -> Result<(), DbError> {
        Ok(())
    }
    fn write_rows(
        &mut self,
        table: &str,
        columns: &[String],
        rows: &[Vec<crate::model::Value>],
    ) -> Result<(), DbError> {
        use crate::export::{write_csv_header, write_csv_row, CsvMode};
        let io = |e: std::io::Error| DbError::new(format!("writing a CSV file failed: {e}"));
        if !self.open.contains_key(table) {
            let path = self.dir.join(Self::file_name(table));
            let mut options = std::fs::OpenOptions::new();
            options.write(true);
            if self.overwrite {
                options.create(true).truncate(true);
            } else {
                options.create_new(true);
            }
            let file = options.open(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    DbError::new(format!(
                        "{} already exists (turn on Drop target objects before create to overwrite it)",
                        Self::file_name(table)
                    ))
                } else {
                    DbError::new(format!("cannot create {}: {e}", Self::file_name(table)))
                }
            })?;
            let mut w = std::io::BufWriter::new(file);
            write_csv_header(&mut w, columns, CsvMode::SpreadsheetSafe).map_err(io)?;
            self.open.insert(table.to_string(), w);
        }
        if let Some(w) = self.open.get_mut(table) {
            for row in rows {
                write_csv_row(w, row, CsvMode::SpreadsheetSafe).map_err(io)?;
            }
        }
        Ok(())
    }
}
