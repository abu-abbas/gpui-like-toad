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
use crate::model::{ColumnDef, IndexDef, ObjectKind, QueryResult};

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
    fn table_exists(&mut self, schema: &str, table: &str) -> Result<bool, DbError>;
    /// Run a SELECT, at most `max_rows` rows, cells up to the export limit.
    fn read(&mut self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
    /// Run statements in order. On failure returns how many succeeded
    /// before it, so the caller can skip exactly the failing statement.
    fn execute_many(&mut self, statements: &[String]) -> Result<u64, (usize, DbError)>;
    fn commit(&mut self) -> Result<(), DbError>;
    fn rollback(&mut self) -> Result<(), DbError>;
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
    let mut cx = Ctx { cancel, emit };
    let mut summary = TransferSummary::default();
    if let Err(e) = spec.validate() {
        cx.log(LogLevel::Error, e.clone());
        summary.invalid = Some(e);
        return summary;
    }
    let total = spec.tables.len();
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
    if cx.cancelled() {
        summary.cancelled = true;
        cx.log(LogLevel::Warn, "Stopped by the user.");
    }
    summary
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
        let sql = sqlgen::create_table_sql(dd, &spec.target_schema, target_name, &plan)?;
        dst.execute_many(&[sql]).map_err(|(_, e)| db(e))?;
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
    Ok(())
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
