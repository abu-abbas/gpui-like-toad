//! Oracle thin-driver implementation of [`ora_core::DbClient`].
//!
//! Uses Oracle's official `oracledb` crate in thin mode: the driver speaks
//! the Oracle Net protocol directly, so end users need no Oracle Client /
//! Instant Client install. The crate is a pre-release (`26.0.0-beta.7` at the
//! pinned revision); its API surface used here is small and covered by the
//! driver's own guide, and the `DbClient` trait isolates the GUI from it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ora_core::completion::CatalogObject;
use ora_core::sql::clean_statement;
mod native_ddl;

use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, PlanNode, QueryResult, RowCollector, SchemaObject, Value,
};

/// Blocking Oracle client. Owned exclusively by the GUI's background DB
/// worker thread; all methods run off the UI thread.
pub struct ThinClient {
    conn: Option<oracledb::Connection>,
    read_only: bool,
    /// `show_storage` of the last `DBMS_METADATA` transform setup on this
    /// session, so repeated DDL views skip the extra round trip.
    ddl_storage: Option<bool>,
}

impl ThinClient {
    /// Run `f` on the connection; on a Read Only profile inside a read
    /// only transaction that is rolled back afterwards.
    fn in_read_scope<T>(
        &mut self,
        f: impl FnOnce(&oracledb::Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let conn = self.conn()?;
        if !self.read_only {
            return f(conn);
        }
        conn.execute("SET TRANSACTION READ ONLY", &[])
            .map_err(db_err)?;
        let result = f(conn);
        let rollback = conn.rollback().map_err(db_err);
        result.and_then(|rows| rollback.map(|()| rows))
    }

    pub fn new() -> Self {
        ThinClient {
            conn: None,
            read_only: false,
            ddl_storage: None,
        }
    }

    fn conn(&self) -> Result<&oracledb::Connection, DbError> {
        self.conn
            .as_ref()
            .ok_or_else(|| DbError::new("not connected"))
    }
}

impl ThinClient {
    fn table_page(
        &mut self,
        owner: &str,
        table: &str,
        filter: &ora_core::TableFilter,
        page: ora_core::TablePage,
        early: Option<Early<'_>>,
    ) -> Result<QueryResult, DbError> {
        let dialect = self.dialect();
        let query = ora_core::filter::select_sql(dialect, owner, table, filter, page);
        let limits = FetchLimits::rows(page.limit);
        let plan = FetchPlan {
            max_previews: if page.rowid { 0 } else { MAX_LOB_PREVIEWS },
            label: "fetch",
        };
        self.in_read_scope(|conn| fetch_rows(conn, &query.sql, &query.binds, limits, plan, early))
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
    row: &mut oracledb::Row,
    ix: usize,
    db_type: &oracledb::DbType,
    rows: &mut LobBudget<'_>,
) -> Result<Value, DbError> {
    use oracledb as odb;
    if db_type == odb::DB_TYPE_CLOB || db_type == odb::DB_TYPE_NCLOB || db_type == odb::DB_TYPE_BLOB
    {
        // Always a locator: the server never sends a whole LOB inline, so
        // reads below are the only place LOB bytes enter memory and they
        // stop at the limits.
        let lob: Option<odb::Lob> = row.take(ix).map_err(db_err)?;
        return match lob {
            Some(mut lob) => lob_value(&mut lob, db_type != odb::DB_TYPE_BLOB, rows),
            None => Ok(Value::Null),
        };
    }
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
        || db_type == odb::DB_TYPE_LONG
        || db_type == odb::DB_TYPE_ROWID
        || db_type == odb::DB_TYPE_UROWID
    {
        // Taken, not copied: a clipped value just drops its tail.
        let max = rows.collector.limits().max_cell_bytes;
        let v: Option<String> = row.take(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |t| Value::owned_text_limited(t, max)))
    } else if db_type == odb::DB_TYPE_RAW || db_type == odb::DB_TYPE_LONG_RAW {
        let max = rows.collector.limits().max_cell_bytes;
        let v: Option<Vec<u8>> = row.take(ix).map_err(db_err)?;
        Ok(v.map_or(Value::Null, |b| Value::owned_bytes_limited(b, max)))
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

/// CLOB cells whose start is read for the grid, per result. Each read is
/// one round trip to the server, so a 1,000-row result of CLOBs must not
/// cost 1,000 of them; later CLOB cells show only their length until
/// "Load full value". BLOBs are never read for the grid: their hex start
/// tells nobody anything.
const MAX_LOB_PREVIEWS: usize = 100;

/// Reads (round trips) spent on one CLOB preview.
const MAX_PREVIEW_READS: usize = 3;

/// State shared by the cells of one fetch.
struct LobBudget<'a> {
    collector: &'a RowCollector,
    previews_left: usize,
    timing: &'a mut FetchTiming,
}

/// Per-stage timing of one fetch. Only written out when timing is switched
/// on (see [`ora_core::timing`]); counting is cheap enough to always run.
#[derive(Default)]
struct FetchTiming {
    /// Time spent waiting in the cursor for the next row (the server round
    /// trips that bring row arrays, `fetch_array_size` rows each).
    next_row: Duration,
    /// Time converting cells that are not LOBs.
    cells: Duration,
    /// Time inside LOB cells: size lookup and preview/full reads.
    lob_total: Duration,
    /// Time spent only in `Lob::read` calls (one server round trip each).
    lob_reads_time: Duration,
    lob_cells: u64,
    lob_read_calls: u64,
    lob_size_calls_ms_max: u128,
    lob_read_ms_max: u128,
    rows: u64,
    /// When the early rows of a streamed fetch were handed over.
    early_ms: Option<u128>,
}

use ora_core::timing::{enabled as timing_enabled, write_line as write_timing_line};

/// A CLOB/BLOB cell from its locator. The length is known without a
/// round trip. Grid fetches read only the start of a CLOB while the
/// preview budget lasts; exports and "Load full value"
/// ([`FetchLimits::read_lobs`]) read the whole value, but never more than
/// the per-cell limit and what the result's byte budget still has room for.
fn lob_value(
    lob: &mut oracledb::Lob,
    is_char: bool,
    budget: &mut LobBudget<'_>,
) -> Result<Value, DbError> {
    let t_size = Instant::now();
    let size = lob.get_size().map_err(db_err)?;
    let size_ms = t_size.elapsed().as_millis();
    budget.timing.lob_size_calls_ms_max = budget.timing.lob_size_calls_ms_max.max(size_ms);
    let unit = if is_char {
        ora_core::LenUnit::Chars
    } else {
        ora_core::LenUnit::Bytes
    };
    let empty = if is_char {
        Value::Text(String::new())
    } else {
        Value::Bytes(Vec::new())
    };
    if size == 0 {
        return Ok(empty);
    }
    let limits = budget.collector.limits();
    let clipped = |preview: Value| {
        Value::Clipped(Box::new(ora_core::ClippedValue {
            preview,
            full_len: size as u64,
            unit,
        }))
    };
    let (max, max_reads) = if limits.read_lobs {
        // Not more than the rest of the result can hold.
        let room = limits
            .max_cell_bytes
            .min(budget.collector.remaining_bytes());
        (room, usize::MAX)
    } else if is_char && budget.previews_left > 0 {
        budget.previews_left -= 1;
        (limits.max_cell_bytes, MAX_PREVIEW_READS)
    } else {
        // A BLOB's hex start tells nobody anything, and later CLOBs show
        // only their length.
        return Ok(clipped(empty));
    };
    let t_read = Instant::now();
    let out = read_lob_bytes(lob, size, is_char, max, max_reads, budget.timing)?;
    let read_elapsed = t_read.elapsed();
    budget.timing.lob_reads_time += read_elapsed;
    budget.timing.lob_read_ms_max = budget.timing.lob_read_ms_max.max(read_elapsed.as_millis());
    if is_char {
        let text = String::from_utf8_lossy(&out).into_owned();
        if text.encode_utf16().count() >= size {
            Ok(Value::Text(text))
        } else {
            Ok(clipped(Value::Text(text)))
        }
    } else if out.len() >= size {
        Ok(Value::Bytes(out))
    } else {
        Ok(clipped(Value::Bytes(out)))
    }
}

/// Read at most `max` bytes of a LOB in at most `max_reads` round trips.
/// A character LOB read needs room for 3 UTF-8 bytes per unit.
fn read_lob_bytes(
    lob: &mut oracledb::Lob,
    size: usize,
    is_char: bool,
    max: usize,
    max_reads: usize,
    timing: &mut FetchTiming,
) -> Result<Vec<u8>, DbError> {
    use std::io::Read as _;
    // Enough for the whole value, never more than `max`.
    let whole = if is_char {
        size.saturating_mul(3)
    } else {
        size
    };
    let mut out: Vec<u8> = Vec::new();
    for _ in 0..max_reads {
        let room = max.saturating_sub(out.len());
        if room < if is_char { 4 } else { 1 } || out.len() >= whole {
            break;
        }
        let mut buf = vec![0u8; room.min(whole - out.len()).clamp(4, READ_CHUNK)];
        timing.lob_read_calls += 1;
        match lob.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            // A later chunk that fails to decode (e.g. a split surrogate
            // pair) just ends the read.
            Err(_) if !out.is_empty() => break,
            Err(err) => return Err(DbError::new(err.to_string())),
        }
    }
    out.truncate(max);
    Ok(out)
}

/// Bytes asked of the server per LOB read.
const READ_CHUNK: usize = 1024 * 1024;

/// Rows asked of the server per round trip. A result of LOB columns has no
/// row prefetch, so every batch is a trip of its own (about 0.3-0.7 s on a
/// VPN); fewer, larger batches matter more than the memory of a few
/// hundred rows. Half the row cap, so the cap plus the one extra row that
/// reveals "more rows exist" is two batches, never three.
fn array_size_for(max_rows: usize) -> u32 {
    (max_rows / 2 + 1).clamp(2, MAX_FETCH_ARRAY_SIZE) as u32
}

/// Upper bound of [`array_size_for`]. Rows are converted (and byte-limited)
/// only after a batch arrived, so this also bounds the transient memory.
const MAX_FETCH_ARRAY_SIZE: usize = 500;

/// How one fetch treats CLOB previews and how it is named in `timing.log`.
#[derive(Clone, Copy)]
struct FetchPlan {
    /// CLOB cells whose start is read with the cursor (one round trip each).
    max_previews: usize,
    label: &'static str,
}

impl FetchPlan {
    const DEFAULT: FetchPlan = FetchPlan {
        max_previews: MAX_LOB_PREVIEWS,
        label: "fetch",
    };
}

/// Rows the data tab asks for with the execute call itself (row prefetch),
/// shown before the rest of the page arrives. The page's other rows still
/// come in arrays of [`array_size_for`], so this costs no extra round trip.
const EARLY_ROWS: usize = 100;

/// A streamed fetch: `call` gets the first `rows` rows once they are in.
struct Early<'a> {
    rows: usize,
    call: &'a mut dyn FnMut(QueryResult),
}

fn fetch_rows(
    conn: &oracledb::Connection,
    sql: &str,
    binds: &[String],
    limits: FetchLimits,
    plan: FetchPlan,
    mut early: Option<Early<'_>>,
) -> Result<QueryResult, DbError> {
    let sql = clean_statement(sql);
    let started = Instant::now();
    let params: Vec<&dyn oracledb::ToDbValue> = binds
        .iter()
        .map(|b| b as &dyn oracledb::ToDbValue)
        .collect();
    // LOBs always come as locators (never inline), so a table of
    // documents never sends whole documents to the client.
    let mut builder = conn
        .statement(sql)
        .map_err(db_err)?
        .fetch_lobs()
        .fetch_array_size(array_size_for(limits.max_rows));
    // Only worth it when the page holds more rows than come early.
    early = early.filter(|e| e.rows > 0 && limits.max_rows > e.rows);
    if let Some(e) = &early {
        builder = builder.prefetch_rows(e.rows as u32);
    }
    let net_before = conn.bytes_received();
    let cursor = builder
        .build()
        .map_err(db_err)?
        .query(&params)
        .map_err(db_err)?;
    let mut collector = RowCollector::started_at(limits, started);
    collector.start_fetch();
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
    let db_types: Vec<&oracledb::DbType> = cursor.columns().iter().map(|m| m.db_type()).collect();
    let t_executed = started.elapsed();
    let mut previews_left = plan.max_previews;
    let mut timing = FetchTiming::default();
    let mut cursor = cursor;
    loop {
        let t_next = Instant::now();
        let Some(row) = cursor.next() else { break };
        timing.next_row += t_next.elapsed();
        let mut row = row.map_err(db_err)?;
        if collector.is_full() {
            break;
        }
        let mut values = Vec::with_capacity(db_types.len());
        let t_cells = Instant::now();
        let mut lob_time = Duration::ZERO;
        {
            let mut budget = LobBudget {
                collector: &collector,
                previews_left,
                timing: &mut timing,
            };
            for (ix, db_type) in db_types.iter().enumerate() {
                let is_lob = *db_type == oracledb::DB_TYPE_CLOB
                    || *db_type == oracledb::DB_TYPE_NCLOB
                    || *db_type == oracledb::DB_TYPE_BLOB;
                let t_cell = Instant::now();
                values.push(cell_value(&mut row, ix, db_type, &mut budget)?);
                if is_lob {
                    lob_time += t_cell.elapsed();
                    budget.timing.lob_cells += 1;
                }
            }
            previews_left = budget.previews_left;
        }
        timing.lob_total += lob_time;
        timing.cells += t_cells.elapsed().saturating_sub(lob_time);
        timing.rows += 1;
        if !collector.push(values) {
            break;
        }
        if early.as_ref().is_some_and(|e| collector.len() == e.rows) {
            if let Some(e) = early.take() {
                timing.early_ms = Some(started.elapsed().as_millis());
                (e.call)(collector.snapshot(columns.clone()));
            }
        }
    }
    let net_bytes = conn.bytes_received().saturating_sub(net_before);
    if timing_enabled() {
        let n = db_types.len();
        // One bare round trip to the server, for comparison.
        let t_ping = Instant::now();
        let ping_ms = conn.ping().ok().map(|()| t_ping.elapsed().as_millis());
        let lobs = db_types
            .iter()
            .filter(|t| {
                **t == oracledb::DB_TYPE_CLOB
                    || **t == oracledb::DB_TYPE_NCLOB
                    || **t == oracledb::DB_TYPE_BLOB
            })
            .count();
        write_timing_line(&format!(
            "{} total_ms={} execute_ms={} next_row_ms={} cells_ms={} lob_cells_ms={} \
             lob_read_ms={} rows={} columns={} lob_columns={} lob_cells={} lob_read_calls={} \
             lob_size_max_ms={} lob_read_max_ms={} fetch_array_size={} ping_ms={} \
             net_kb={} net_kbps={} early_ms={}",
            plan.label,
            started.elapsed().as_millis(),
            t_executed.as_millis(),
            timing.next_row.as_millis(),
            timing.cells.as_millis(),
            timing.lob_total.as_millis(),
            timing.lob_reads_time.as_millis(),
            timing.rows,
            n,
            lobs,
            timing.lob_cells,
            timing.lob_read_calls,
            timing.lob_size_calls_ms_max,
            timing.lob_read_ms_max,
            array_size_for(limits.max_rows),
            ping_ms.map_or_else(|| "-".to_string(), |m| m.to_string()),
            net_bytes / 1024,
            // Received bytes over the time spent waiting for them: about
            // the link's speed when the result is large.
            (net_bytes as u128 * 1000 / 1024)
                .checked_div((t_executed + timing.next_row).as_millis())
                .unwrap_or(0),
            timing
                .early_ms
                .map_or_else(|| "-".to_string(), |m| m.to_string()),
        ));
    }
    Ok(collector.finish(columns))
}

/// Characters of a CLOB the data tab shows before "Load full value":
/// [`PREVIEW_PIECES`] SQL-side substrings of [`PREVIEW_PIECE_CHARS`] each
/// (a SQL string result is at most 4000 bytes, and a character takes up to
/// 4).
const PREVIEW_PIECE_CHARS: usize = 1000;
const PREVIEW_PIECES: usize = 2;

/// ROWIDs per preview query (an `IN` list holds at most 1000 entries).
const PREVIEW_ROWIDS_PER_QUERY: usize = 400;

/// `SELECT ROWIDTOCHAR(ROWID), <pieces of each column> FROM owner.table
/// WHERE ROWID IN (...)`: the starts of many CLOB cells in one round trip,
/// instead of one `LOB.read` trip per cell.
fn preview_sql(
    dialect: &dyn ora_core::Dialect,
    owner: &str,
    table: &str,
    columns: &[&str],
    rowids: usize,
) -> String {
    let mut select = String::from("ROWIDTOCHAR(ROWID)");
    for column in columns {
        let q = dialect.quote_ident(column);
        for piece in 0..PREVIEW_PIECES {
            select.push_str(&format!(
                ", DBMS_LOB.SUBSTR({q}, {PREVIEW_PIECE_CHARS}, {})",
                piece * PREVIEW_PIECE_CHARS + 1
            ));
        }
    }
    let ids: Vec<String> = (1..=rowids).map(|i| format!("CHARTOROWID(:{i})")).collect();
    format!(
        "SELECT {select} FROM {} WHERE ROWID IN ({})",
        dialect.table_ref(owner, table),
        ids.join(", ")
    )
}

/// The previews in the rows of a [`preview_sql`] query: ROWID text, then
/// [`PREVIEW_PIECES`] pieces per column in `columns` order. NULL and empty
/// cells are left out.
fn previews_from_rows(columns: &[&str], rows: Vec<Vec<Value>>) -> Vec<ora_core::ClobPreview> {
    let mut out = Vec::new();
    for row in rows {
        let Some(Value::Text(rowid)) = row.first() else {
            continue;
        };
        for (n, column) in columns.iter().enumerate() {
            let start = 1 + n * PREVIEW_PIECES;
            let mut text = String::new();
            for piece in row.iter().skip(start).take(PREVIEW_PIECES) {
                if let Value::Text(t) = piece {
                    text.push_str(t);
                }
            }
            if !text.is_empty() {
                out.push(ora_core::ClobPreview {
                    rowid: rowid.clone(),
                    column: (*column).to_string(),
                    text,
                });
            }
        }
    }
    out
}

impl DbClient for ThinClient {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn connect(&mut self, profile: &ConnectionProfile, password: &str) -> Result<DbInfo, DbError> {
        if profile.access.read_only
            && (profile.access.sysdba || profile.username.eq_ignore_ascii_case("SYS"))
        {
            return Err(DbError::new("Read Only requires a non-SYS connection."));
        }
        // No client-side statement cache: after one failed statement a cached
        // cursor could keep failing with ORA-01008 / ORA-01002 for every later
        // query of the session until reconnect.
        let mut config = oracledb::Config::default()
            .set_credentials(&profile.username, password)
            .set_connect_string(&profile.connect_string)
            .map_err(db_err)?
            .set_stmtcachesize(0);
        if profile.access.sysdba {
            config = config.set_auth_mode(oracledb::AUTH_MODE_SYSDBA);
        }
        let conn = oracledb::connect(config).map_err(db_err)?;
        let user_row = conn
            .query_row("SELECT USER FROM DUAL", &[])
            .map_err(db_err)?;
        let user: String = user_row.get(0).map_err(db_err)?;
        let version = conn.version().map(|v| v.to_string()).unwrap_or_default();
        let database = conn.db_name().unwrap_or("").to_string();
        self.read_only = profile.access.read_only;
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

    fn ping(&self) -> Result<(), DbError> {
        let conn = self.conn()?;
        let timeout = conn.call_timeout().map_err(db_err)?;
        conn.set_call_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(db_err)?;
        let result = conn.ping().map_err(db_err);
        let reset = conn.set_call_timeout(timeout).map_err(db_err);
        result.and(reset)
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<QueryResult, DbError> {
        self.query_binds(sql, &[], limits)
    }

    fn query_binds(
        &mut self,
        sql: &str,
        binds: &[String],
        limits: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        self.in_read_scope(|conn| fetch_rows(conn, sql, binds, limits, FetchPlan::DEFAULT, None))
    }

    /// The data tab's page. CLOB cells are not read while the cursor runs
    /// (a trip each, painfully slow over a VPN): the page comes back with
    /// their lengths, and the grid asks for the starts of the rows it
    /// shows with [`DbClient::fetch_clob_previews`].
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &ora_core::TableFilter,
        page: ora_core::TablePage,
    ) -> Result<QueryResult, DbError> {
        self.table_page(owner, table, filter, page, None)
    }

    /// [`DbClient::query_table`] with the first [`EARLY_ROWS`] rows handed
    /// to `first` as soon as the execute round trip brought them.
    fn query_table_streamed(
        &mut self,
        database: Option<&str>,
        owner: &str,
        table: &str,
        filter: &ora_core::TableFilter,
        page: ora_core::TablePage,
        first: &mut dyn FnMut(QueryResult),
    ) -> Result<QueryResult, DbError> {
        if database.is_some() {
            return self.query_table_in(database, owner, table, filter, page);
        }
        let early = Early {
            rows: EARLY_ROWS,
            call: first,
        };
        self.table_page(owner, table, filter, page, Some(early))
    }

    fn fetch_clob_previews(
        &mut self,
        owner: &str,
        table: &str,
        columns: &[String],
        rowids: &[String],
    ) -> Result<Vec<ora_core::ClobPreview>, DbError> {
        if columns.is_empty() || rowids.is_empty() {
            return Ok(Vec::new());
        }
        let dialect = self.dialect();
        let names: Vec<&str> = columns.iter().map(String::as_str).collect();
        let mut previews = Vec::new();
        self.in_read_scope(|conn| {
            for chunk in rowids.chunks(PREVIEW_ROWIDS_PER_QUERY) {
                let sql = preview_sql(dialect, owner, table, &names, chunk.len());
                let plan = FetchPlan {
                    max_previews: 0,
                    label: "preview-fetch",
                };
                let part = fetch_rows(
                    conn,
                    &sql,
                    chunk,
                    FetchLimits::unlimited_bytes(chunk.len()),
                    plan,
                    None,
                )?;
                previews.extend(previews_from_rows(&names, part.rows));
            }
            Ok(previews)
        })
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let result = conn.execute(sql, &[]).map_err(db_err)?;
        Ok(result.rows_affected())
    }

    fn execute_binds(&mut self, sql: &str, binds: &[String]) -> Result<u64, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        let params: Vec<&dyn oracledb::ToDbValue> = binds
            .iter()
            .map(|b| b as &dyn oracledb::ToDbValue)
            .collect();
        let result = conn.execute(sql, &params).map_err(db_err)?;
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
                // A materialized view is listed twice by the dictionary: as
                // MATERIALIZED VIEW and as its container TABLE. Hide the
                // container table so the MV shows up in its own category only.
                // Database links are not in ALL_OBJECTS at all; they come from
                // ALL_DB_LINKS (own links plus PUBLIC ones, so only the
                // schema's own are kept).
                "SELECT owner, object_name, object_type, parent, detail FROM (\
                 SELECT o.owner, o.object_name, o.object_type, \
                 CAST(NULL AS VARCHAR2(128)) AS parent, CAST(NULL AS VARCHAR2(60)) AS detail \
                 FROM all_objects o \
                 WHERE o.owner = :1 AND o.object_type IN \
                 ('TABLE','VIEW','MATERIALIZED VIEW','INDEX','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TRIGGER','SYNONYM','TYPE') \
                 AND NOT (o.object_type = 'TYPE' AND o.object_name LIKE 'SYS\\_PLSQL\\_%' ESCAPE '\\') \
                 AND o.object_name NOT LIKE 'BIN$%' \
                 AND NOT (o.object_type = 'TABLE' AND EXISTS \
                   (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name)) \
                 UNION ALL \
                 SELECT l.owner, l.db_link, 'DATABASE LINK', NULL, NULL FROM all_db_links l WHERE l.owner = :2 \
                 UNION ALL \
                 SELECT c.owner, c.constraint_name, 'CONSTRAINT', c.table_name, \
                 CASE c.constraint_type WHEN 'P' THEN 'PRIMARY KEY' WHEN 'U' THEN 'UNIQUE' \
                 WHEN 'R' THEN 'FOREIGN KEY' ELSE 'CHECK' END || ', ' || c.status \
                 FROM all_constraints c \
                 WHERE c.owner = :3 AND c.constraint_type IN ('P','U','R','C') \
                 AND NOT (c.constraint_type = 'C' AND c.generated = 'GENERATED NAME') \
                 AND c.constraint_name NOT LIKE 'BIN$%') \
                 ORDER BY object_type, object_name",
                &[&owner_param, &owner_param, &owner_param],
            )
            .map_err(db_err)?;
        let mut objects = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let owner: String = row.get(0).map_err(db_err)?;
            let name: String = row.get(1).map_err(db_err)?;
            let raw_type: String = row.get(2).map_err(db_err)?;
            let parent: Option<String> = row.get(3).map_err(db_err)?;
            let detail: Option<String> = row.get(4).map_err(db_err)?;
            objects.push(SchemaObject {
                owner,
                name,
                kind: ObjectKind::from_oracle_type(&raw_type),
                parent,
                detail,
            });
        }
        // The recycle bin is best effort: a user without access to the view
        // (or a database that has it switched off) still gets every other
        // category. USER_RECYCLEBIN is the session user's own bin, so other
        // schemas show none.
        if let Ok(cursor) = conn.query(
            "SELECT object_name, original_name, type, operation, droptime, \
             can_undrop, can_purge FROM user_recyclebin \
             WHERE :1 = SYS_CONTEXT('USERENV', 'SESSION_USER') ORDER BY droptime DESC",
            &[&owner_param],
        ) {
            for row in cursor.flatten() {
                let text = |ix: usize| row.get::<Option<String>>(ix).ok().flatten();
                let (Some(bin_name), Some(original)) = (text(0), text(1)) else {
                    continue;
                };
                let info = ora_core::recycle::RecycleInfo {
                    kind: text(2).unwrap_or_default(),
                    operation: text(3).unwrap_or_else(|| "DROP".to_string()),
                    dropped: ora_core::recycle::pretty_droptime(&text(4).unwrap_or_default()),
                    can_undrop: text(5).as_deref() == Some("YES"),
                    can_purge: text(6).as_deref() == Some("YES"),
                };
                objects.push(SchemaObject {
                    owner: owner_upper.clone(),
                    name: bin_name,
                    kind: ObjectKind::RecycleBin,
                    parent: Some(original),
                    detail: Some(info.to_detail()),
                });
            }
        }
        Ok(objects)
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let cursor = conn
            .query("SELECT username FROM all_users ORDER BY username", &[])
            .map_err(db_err)?;
        let mut schemas = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            schemas.push(name);
        }
        Ok(schemas)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        self.object_ddl_with(owner, name, kind, false)
    }

    fn object_ddl_with(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
        show_storage: bool,
    ) -> Result<String, DbError> {
        let total = std::time::Instant::now();
        let conn = self.conn()?;
        // Plain objects come straight from the dictionary: DBMS_METADATA's
        // first call per object can take 10+ s on a big production
        // dictionary. "Show storage" always asks DBMS_METADATA.
        if !show_storage {
            if let Some(ddl) = native_ddl::native_ddl(conn, owner, name, kind) {
                if timing_enabled() {
                    write_timing_line(&format!(
                        "ddl native=1 total_ms={} bytes={} lines={}",
                        total.elapsed().as_millis(),
                        ddl.len(),
                        ddl.lines().count(),
                    ));
                }
                return Ok(ddl);
            }
        }
        // Session-scoped DBMS_METADATA setup (terminator, pretty print and
        // the storage noise switch). Only re-sent when the choice changed.
        let t_setup = std::time::Instant::now();
        let mut setup_calls = 0u32;
        if self.ddl_storage != Some(show_storage) {
            conn.execute(&ddl_transform_sql(show_storage), &[])
                .map_err(db_err)?;
            setup_calls = 1;
        }
        let setup_ms = t_setup.elapsed().as_millis();
        // DBMS_METADATA spells multi-word types with an underscore
        // (MATERIALIZED_VIEW).
        let type_owned = match kind {
            ObjectKind::DbLink => "DB_LINK".to_string(),
            // Foreign keys have their own metadata type.
            ObjectKind::Constraint => {
                let owner_param: &str = owner;
                let name_param: &str = name;
                let is_ref = conn
                    .query(
                        "SELECT constraint_type FROM all_constraints \
                         WHERE owner = :1 AND constraint_name = :2",
                        &[&owner_param, &name_param],
                    )
                    .map_err(db_err)?
                    .next()
                    .and_then(|row| row.ok())
                    .and_then(|row| row.get::<Option<String>>(0).ok().flatten())
                    .is_some_and(|t| t == "R");
                if is_ref {
                    "REF_CONSTRAINT"
                } else {
                    "CONSTRAINT"
                }
                .to_string()
            }
            other => other.oracle_type().replace(' ', "_"),
        };
        let type_param: &str = &type_owned;
        let name_param: &str = name;
        let owner_param: &str = owner;
        let t_get = std::time::Instant::now();
        let cursor = conn
            .query(
                "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                &[&type_param, &name_param, &owner_param],
            )
            .map_err(db_err)?;
        let query_ms = t_get.elapsed().as_millis();
        let t_read = std::time::Instant::now();
        let Some(row) = cursor.into_iter().next() else {
            return Err(DbError::new(format!("No DDL returned for {owner}.{name}")));
        };
        let ddl: Option<String> = row.map_err(db_err)?.get(0).map_err(db_err)?;
        let read_ms = t_read.elapsed().as_millis();
        let mut ddl = ddl.unwrap_or_default().trim().to_string();
        // GET_DDL('TYPE') is the spec only; add the body when there is one.
        let t_body = std::time::Instant::now();
        if matches!(kind, ObjectKind::Type) {
            let body_type: &str = "TYPE_BODY";
            if let Ok(cursor) = conn.query(
                "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                &[&body_type, &name_param, &owner_param],
            ) {
                let body = cursor
                    .into_iter()
                    .next()
                    .and_then(|row| row.ok())
                    .and_then(|row| row.get::<Option<String>>(0).ok().flatten());
                if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
                    ddl = format!("{ddl}\n\n{}", body.trim());
                }
            }
        }
        let ddl_len = ddl.len();
        // `conn` is no longer used: remember the transform setup.
        self.ddl_storage = Some(show_storage);
        if timing_enabled() {
            write_timing_line(&format!(
                "ddl native=0 total_ms={} setup_ms={setup_ms} setup_calls={setup_calls} \
                 query_ms={query_ms} read_ms={read_ms} body_ms={} bytes={} lines={} \
                 show_storage={}",
                total.elapsed().as_millis(),
                t_body.elapsed().as_millis(),
                ddl_len,
                ddl.lines().count(),
                u8::from(show_storage),
            ));
        }
        Ok(ddl)
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

    fn catalog_objects(&mut self, owner: &str) -> Result<Vec<CatalogObject>, DbError> {
        // Two dictionary queries for the whole schema instead of one
        // describe per table: a real schema can hold thousands of tables.
        let objects = self.list_objects(owner)?;
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let cursor = conn
            .query(
                "SELECT table_name, column_name, data_type, data_length, data_precision, \
                 data_scale, nullable FROM all_tab_columns \
                 WHERE owner = :1 ORDER BY table_name, column_id",
                &[&owner_param],
            )
            .map_err(db_err)?;
        let mut columns: HashMap<String, Vec<ColumnDef>> = HashMap::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let table: String = row.get(0).map_err(db_err)?;
            let name: String = row.get(1).map_err(db_err)?;
            let data_type: String = row.get(2).map_err(db_err)?;
            let length: Option<oracledb::OracleNumber> = row.get(3).map_err(db_err)?;
            let precision: Option<oracledb::OracleNumber> = row.get(4).map_err(db_err)?;
            let scale: Option<oracledb::OracleNumber> = row.get(5).map_err(db_err)?;
            let nullable_flag: String = row.get(6).map_err(db_err)?;
            columns.entry(table).or_default().push(ColumnDef {
                name,
                data_type: describe_type(&data_type, length, precision, scale),
                nullable: nullable_flag == "Y",
                default: None,
            });
        }
        Ok(objects
            .into_iter()
            .map(|object| CatalogObject {
                columns: match object.kind {
                    ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                        columns.remove(&object.name).unwrap_or_default()
                    }
                    _ => Vec::new(),
                },
                name: object.name,
                kind: object.kind,
            })
            .collect())
    }

    fn list_indexes(&mut self, owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let table_upper = table.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let table_param: &str = &table_upper;
        // One row per index column, in key order; grouped below. (No
        // LISTAGG: it raises ORA-01489 on very wide keys.)
        let cursor = conn
            .query(
                "SELECT i.index_name, i.uniqueness, i.index_type, i.status, c.column_name \
                 FROM all_indexes i \
                 LEFT JOIN all_ind_columns c \
                   ON c.index_owner = i.owner AND c.index_name = i.index_name \
                 WHERE i.table_owner = :1 AND i.table_name = :2 \
                 ORDER BY i.index_name, c.column_position",
                &[&owner_param, &table_param],
            )
            .map_err(db_err)?;
        let mut indexes: Vec<IndexDef> = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            let uniqueness: Option<String> = row.get(1).map_err(db_err)?;
            let index_type: Option<String> = row.get(2).map_err(db_err)?;
            let status: Option<String> = row.get(3).map_err(db_err)?;
            let column: Option<String> = row.get(4).map_err(db_err)?;
            if indexes.last().map(|i| i.name.as_str()) != Some(name.as_str()) {
                indexes.push(IndexDef {
                    name,
                    unique: uniqueness.as_deref() == Some("UNIQUE"),
                    index_type: index_type.unwrap_or_default(),
                    columns: Vec::new(),
                    status: status.unwrap_or_default(),
                });
            }
            if let (Some(index), Some(column)) = (indexes.last_mut(), column) {
                index.columns.push(column);
            }
        }
        Ok(indexes)
    }

    fn list_constraints(
        &mut self,
        owner: &str,
        table: &str,
    ) -> Result<Vec<ora_core::ConstraintDef>, DbError> {
        use ora_core::{ConstraintDef, ConstraintKind};
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let table_upper = table.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let table_param: &str = &table_upper;
        // One row per constraint column, in key order; grouped below.
        let cursor = conn
            .query(
                "SELECT c.constraint_name, c.constraint_type, c.search_condition, \
                        c.r_owner, c.r_constraint_name, c.delete_rule, cc.column_name \
                 FROM all_constraints c \
                 LEFT JOIN all_cons_columns cc \
                   ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
                 WHERE c.owner = :1 AND c.table_name = :2 \
                   AND c.constraint_type IN ('P', 'U', 'R', 'C') AND c.status = 'ENABLED' \
                 ORDER BY c.constraint_name, cc.position",
                &[&owner_param, &table_param],
            )
            .map_err(db_err)?;
        let mut out: Vec<ConstraintDef> = Vec::new();
        let mut refs: Vec<Option<(String, String)>> = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let name: String = row.get(0).map_err(db_err)?;
            let kind: String = row.get(1).map_err(db_err)?;
            let condition: Option<String> = row.get(2).map_err(db_err)?;
            let r_owner: Option<String> = row.get(3).map_err(db_err)?;
            let r_name: Option<String> = row.get(4).map_err(db_err)?;
            let delete_rule: Option<String> = row.get(5).map_err(db_err)?;
            let column: Option<String> = row.get(6).map_err(db_err)?;
            if out.last().map(|c| c.name.as_str()) != Some(name.as_str()) {
                out.push(ConstraintDef {
                    name,
                    kind: match kind.as_str() {
                        "P" => ConstraintKind::PrimaryKey,
                        "U" => ConstraintKind::Unique,
                        "R" => ConstraintKind::ForeignKey,
                        _ => ConstraintKind::Check,
                    },
                    columns: Vec::new(),
                    ref_table: String::new(),
                    ref_columns: Vec::new(),
                    on_delete: delete_rule.unwrap_or_default(),
                    condition: condition.map(|c| c.trim().to_string()).unwrap_or_default(),
                });
                refs.push(r_owner.zip(r_name));
            }
            if let (Some(c), Some(column)) = (out.last_mut(), column) {
                c.columns.push(column);
            }
        }
        // NOT NULL is a CHECK in the dictionary; the column flag carries it.
        let mut keep = Vec::new();
        for (c, r) in out.into_iter().zip(refs) {
            if c.kind == ConstraintKind::Check {
                let upper = c.condition.to_ascii_uppercase();
                if c.columns.len() == 1
                    && upper.ends_with("IS NOT NULL")
                    && !upper.contains(" AND ")
                    && !upper.contains(" OR ")
                {
                    continue;
                }
            }
            keep.push((c, r));
        }
        let mut result = Vec::with_capacity(keep.len());
        for (mut c, r) in keep {
            if let Some((r_owner, r_name)) = r.filter(|_| c.kind == ConstraintKind::ForeignKey) {
                let (ro, rn): (&str, &str) = (&r_owner, &r_name);
                let cursor = conn
                    .query(
                        "SELECT table_name, column_name FROM all_cons_columns \
                         WHERE owner = :1 AND constraint_name = :2 ORDER BY position",
                        &[&ro, &rn],
                    )
                    .map_err(db_err)?;
                for row in cursor {
                    let row = row.map_err(db_err)?;
                    let t: String = row.get(0).map_err(db_err)?;
                    let col: String = row.get(1).map_err(db_err)?;
                    c.ref_table = if r_owner.eq_ignore_ascii_case(&owner_upper) {
                        t
                    } else {
                        format!("{r_owner}.{t}")
                    };
                    c.ref_columns.push(col);
                }
            }
            result.push(c);
        }
        Ok(result)
    }

    fn trigger_scripts(&mut self, owner: &str, table: &str) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let table_upper = table.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let table_param: &str = &table_upper;
        let cursor = conn
            .query(
                "SELECT owner, trigger_name FROM all_triggers \
                 WHERE table_owner = :1 AND table_name = :2 AND base_object_type = 'TABLE' \
                 ORDER BY trigger_name",
                &[&owner_param, &table_param],
            )
            .map_err(db_err)?;
        let mut names = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let o: String = row.get(0).map_err(db_err)?;
            let n: String = row.get(1).map_err(db_err)?;
            names.push((o, n));
        }
        let mut out = Vec::new();
        for (o, n) in names {
            let (kind, name_param, owner_param): (&str, &str, &str) = ("TRIGGER", &n, &o);
            let cursor = conn
                .query(
                    "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                    &[&kind, &name_param, &owner_param],
                )
                .map_err(db_err)?;
            let Some(row) = cursor.into_iter().next() else {
                continue;
            };
            let ddl: Option<String> = row.map_err(db_err)?.get(0).map_err(db_err)?;
            let ddl = ddl.unwrap_or_default();
            // GET_DDL appends `ALTER TRIGGER ... ENABLE`; new triggers are
            // enabled anyway and it is a second statement.
            let cut = ddl
                .to_ascii_uppercase()
                .rfind("\nALTER TRIGGER")
                .unwrap_or(ddl.len());
            let text = ddl[..cut].trim().to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
        Ok(out)
    }

    fn routine_signature(
        &mut self,
        owner: &str,
        name: &str,
    ) -> Result<(Vec<ora_core::ddl::RoutineArg>, String), DbError> {
        let conn = self.conn()?;
        let owner_upper = owner.trim().to_ascii_uppercase();
        let owner_param: &str = &owner_upper;
        let name_param: &str = name;
        // Position 0 is the return value; data_level 0 skips the fields of
        // record parameters.
        let cursor = conn
            .query(
                "SELECT argument_name, data_type, in_out, position \
                 FROM all_arguments \
                 WHERE owner = :1 AND object_name = :2 AND package_name IS NULL \
                   AND data_level = 0 AND data_type IS NOT NULL \
                 ORDER BY position",
                &[&owner_param, &name_param],
            )
            .map_err(db_err)?;
        let mut args = Vec::new();
        let mut returns = String::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let arg_name: Option<String> = row.get(0).map_err(db_err)?;
            let data_type: Option<String> = row.get(1).map_err(db_err)?;
            let in_out: Option<String> = row.get(2).map_err(db_err)?;
            match arg_name {
                None => returns = data_type.unwrap_or_default(),
                Some(arg_name) => args.push(ora_core::ddl::RoutineArg {
                    name: arg_name,
                    data_type: data_type.unwrap_or_default(),
                    direction: in_out.unwrap_or_else(|| "IN".to_string()),
                }),
            }
        }
        Ok((args, returns))
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        run_explain_plan(conn, sql)?;
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

    fn explain_plan_nodes(&mut self, sql: &str) -> Result<Vec<PlanNode>, DbError> {
        let conn = self.conn()?;
        let sql = clean_statement(sql);
        run_explain_plan(conn, sql)?;
        // Each EXPLAIN PLAN replaces the rows for its (null) statement id,
        // so this query sees only the plan just generated. The hierarchical
        // walk returns rows in tree order, siblings ordered by POSITION.
        let cursor = conn
            .query(
                "SELECT id, parent_id, operation, options, object_name, cost, cardinality \
                 FROM plan_table START WITH id = 0 \
                 CONNECT BY PRIOR id = parent_id ORDER SIBLINGS BY position",
                &[],
            )
            .map_err(db_err)?;
        let mut nodes = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let id: Option<oracledb::OracleNumber> = row.get(0).map_err(db_err)?;
            let parent_id: Option<oracledb::OracleNumber> = row.get(1).map_err(db_err)?;
            let operation: Option<String> = row.get(2).map_err(db_err)?;
            let options: Option<String> = row.get(3).map_err(db_err)?;
            let object_name: Option<String> = row.get(4).map_err(db_err)?;
            let cost: Option<oracledb::OracleNumber> = row.get(5).map_err(db_err)?;
            let cardinality: Option<oracledb::OracleNumber> = row.get(6).map_err(db_err)?;
            nodes.push(PlanNode {
                id: id.and_then(number_to_i64).unwrap_or(0),
                parent_id: parent_id.and_then(number_to_i64),
                operation: operation.unwrap_or_default(),
                options: options.unwrap_or_default(),
                object_name: object_name.unwrap_or_default(),
                cost: cost.and_then(number_to_i64),
                cardinality: cardinality.and_then(number_to_i64),
            });
        }
        Ok(nodes)
    }
}

/// Run `EXPLAIN PLAN FOR <sql>`, translating a missing PLAN_TABLE into an
/// error that tells the user how to create it.
fn run_explain_plan(conn: &oracledb::Connection, sql: &str) -> Result<(), DbError> {
    if let Err(err) = conn.execute(&format!("EXPLAIN PLAN FOR {sql}"), &[]) {
        let mut message = err.to_string();
        if message.contains("02404") || message.contains("PLAN_TABLE") {
            message.push_str(
                "\nHint: Run @?/rdbms/admin/catplan.sql as this user \
                 to create PLAN_TABLE.",
            );
        }
        return Err(DbError::new(message));
    }
    Ok(())
}

/// `PLAN_TABLE` cost/cardinality are NUMBERs; the UI only needs whole
/// values, and the optimizer reports them as integers. Non-integer or
/// unparseable values become `None` rather than failing the whole plan.
fn number_to_i64(n: oracledb::OracleNumber) -> Option<i64> {
    n.to_string().parse::<i64>().ok()
}

/// PL/SQL block that sets the session's `DBMS_METADATA` transforms for
/// View Script / the DDL tab: statements end with `;`, output is pretty
/// printed, and unless `show_storage` the physical noise (STORAGE clauses,
/// segment attributes such as PCTFREE/LOGGING, TABLESPACE) is left out.
fn ddl_transform_sql(show_storage: bool) -> String {
    let flag = if show_storage { "TRUE" } else { "FALSE" };
    let mut sql = String::from("BEGIN ");
    for (param, value) in [
        ("SQLTERMINATOR", "TRUE"),
        ("PRETTY", "TRUE"),
        ("SEGMENT_ATTRIBUTES", flag),
        ("STORAGE", flag),
        ("TABLESPACE", flag),
    ] {
        sql.push_str(&format!(
            "DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, '{param}', {value}); "
        ));
    }
    sql.push_str("END;");
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn array_size_is_half_the_row_cap_and_bounded() {
        assert_eq!(array_size_for(1000), 500);
        assert_eq!(array_size_for(100), 51);
        assert_eq!(array_size_for(1), 2);
        assert_eq!(array_size_for(1_000_000), 500);
    }

    #[test]
    fn preview_sql_reads_pieces_of_every_clob_column_by_rowid() {
        let sql = preview_sql(&ora_core::dialect::OracleDialect, "HR", "DOC", &["BODY"], 2);
        assert_eq!(
            sql,
            "SELECT ROWIDTOCHAR(ROWID), DBMS_LOB.SUBSTR(\"BODY\", 1000, 1), \
             DBMS_LOB.SUBSTR(\"BODY\", 1000, 1001) FROM \"HR\".\"DOC\" \
             WHERE ROWID IN (CHARTOROWID(:1), CHARTOROWID(:2))"
        );
    }

    #[test]
    fn previews_join_pieces_and_skip_null_cells() {
        let rows = vec![
            vec![
                text("R2"),
                text("abc"),
                text("def"),
                Value::Null,
                Value::Null,
            ],
            vec![text("R1"), text("hello"), Value::Null, text("x"), text("y")],
            vec![
                Value::Null,
                text("no rowid"),
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        ];
        let got = previews_from_rows(&["A", "B"], rows);
        let pair = |r: &str, c: &str, t: &str| ora_core::ClobPreview {
            rowid: r.into(),
            column: c.into(),
            text: t.into(),
        };
        assert_eq!(
            got,
            vec![
                pair("R2", "A", "abcdef"),
                pair("R1", "A", "hello"),
                pair("R1", "B", "xy")
            ]
        );
    }

    #[test]
    fn thin_client_starts_disconnected() {
        let mut client = ThinClient::new();
        assert!(!client.is_connected());
        assert!(client
            .query("SELECT 1 FROM DUAL", FetchLimits::rows(10))
            .is_err());
        assert!(client.explain_plan_nodes("SELECT 1 FROM DUAL").is_err());
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

    #[test]
    fn connect_string_validation_rejects_blank_and_garbage() {
        let blank = oracledb::Config::default().set_connect_string("");
        assert!(blank.is_err(), "empty connect string should not parse");
        let garbage = oracledb::Config::default().set_connect_string("::::");
        assert!(garbage.is_err(), "garbage connect string should not parse");
    }

    #[test]
    fn thin_client_connect_reports_refused_connection() {
        let mut client = ThinClient::new();
        let err = client
            .connect(
                &ora_core::ConnectionProfile {
                    name: "refused".into(),
                    kind: DbKind::Oracle,
                    username: "hr".into(),
                    connect_string: "127.0.0.1:1/nonexistent".into(),
                    access: Default::default(),
                },
                "password",
            )
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(!client.is_connected());
    }

    #[test]
    fn ddl_transform_hides_storage_by_default() {
        let lean = ddl_transform_sql(false);
        for param in ["SEGMENT_ATTRIBUTES", "STORAGE", "TABLESPACE"] {
            assert!(lean.contains(&format!("'{param}', FALSE")), "{param}");
        }
        assert!(lean.contains("'SQLTERMINATOR', TRUE"));
        assert!(lean.contains("'PRETTY', TRUE"));
        assert!(lean.starts_with("BEGIN ") && lean.ends_with("END;"));
    }

    #[test]
    fn ddl_transform_can_show_storage() {
        let full = ddl_transform_sql(true);
        assert!(!full.contains("FALSE"));
        assert!(full.contains("'STORAGE', TRUE"));
    }
}
