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
use ora_core::{
    ColumnDef, ColumnInfo, ConnectionProfile, DbClient, DbError, DbInfo, DbKind, FetchLimits,
    IndexDef, ObjectKind, PlanNode, QueryResult, RowCollector, SchemaObject, Value,
};

/// Blocking Oracle client. Owned exclusively by the GUI's background DB
/// worker thread; all methods run off the UI thread.
pub struct ThinClient {
    conn: Option<oracledb::Connection>,
    read_only: bool,
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
        }
    }

    fn conn(&self) -> Result<&oracledb::Connection, DbError> {
        self.conn
            .as_ref()
            .ok_or_else(|| DbError::new("not connected"))
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
/// on (see [`timing_enabled`]); counting is cheap enough to always run.
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
}

/// Timing is opt-in: set the environment variable `KILAT_TIMING=1`, or
/// create an empty file named `timing-on` in the config directory (handy
/// for a macOS `.app` started from Finder).
fn timing_enabled() -> bool {
    std::env::var_os("KILAT_TIMING").is_some_and(|v| !v.is_empty() && v != "0")
        || ora_core::store::config_dir().join("timing-on").exists()
}

/// Append one line to `timing.log` in the config directory. Carries only
/// counts and durations: no SQL text, no cell contents, no names.
fn write_timing_line(line: &str) {
    use std::io::Write as _;
    let dir = ora_core::store::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("timing.log"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} {line}");
    }
}

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

fn fetch_rows(
    conn: &oracledb::Connection,
    sql: &str,
    binds: &[String],
    limits: FetchLimits,
    plan: FetchPlan,
) -> Result<QueryResult, DbError> {
    let sql = clean_statement(sql);
    let started = Instant::now();
    let params: Vec<&dyn oracledb::ToDbValue> = binds
        .iter()
        .map(|b| b as &dyn oracledb::ToDbValue)
        .collect();
    // LOBs always come as locators (never inline), so a table of
    // documents never sends whole documents to the client.
    let cursor = conn
        .statement(sql)
        .map_err(db_err)?
        .fetch_lobs()
        .fetch_array_size(array_size_for(limits.max_rows))
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
    }
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
             lob_size_max_ms={} lob_read_max_ms={} fetch_array_size={} ping_ms={}",
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

/// A CLOB cell the cursor left without a preview: clipped, empty, counted
/// in characters.
fn is_unread_clob(value: &Value) -> bool {
    matches!(
        value,
        Value::Clipped(c)
            if c.unit == ora_core::LenUnit::Chars
                && matches!(&c.preview, Value::Text(t) if t.is_empty())
    )
}

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

/// Put the previews read by [`preview_sql`] into the clipped cells of
/// `result`. `rows` are the preview query's rows: ROWID text, then
/// [`PREVIEW_PIECES`] pieces per column in `columns` order. Cells whose
/// start would push the result over `max_bytes` stay empty.
fn merge_previews(
    result: &mut QueryResult,
    rowid_ix: usize,
    columns: &[usize],
    rows: Vec<Vec<Value>>,
    max_bytes: usize,
) {
    let mut found: HashMap<String, Vec<Vec<Value>>> = HashMap::new();
    for mut row in rows {
        if row.is_empty() {
            continue;
        }
        if let Value::Text(id) = row.remove(0) {
            found.insert(id, vec![row]);
        }
    }
    let mut used = result.heap_bytes();
    for row in &mut result.rows {
        let Some(Value::Text(id)) = row.get(rowid_ix) else {
            continue;
        };
        let Some(pieces) = found.get(id).and_then(|v| v.first()) else {
            continue;
        };
        for (n, &col) in columns.iter().enumerate() {
            let Value::Clipped(clipped) = &row[col] else {
                continue;
            };
            if !is_unread_clob(&row[col]) {
                continue;
            }
            let mut text = String::new();
            for piece in &pieces[n * PREVIEW_PIECES..(n + 1) * PREVIEW_PIECES] {
                if let Value::Text(t) = piece {
                    text.push_str(t);
                }
            }
            if text.is_empty() || used + text.len() > max_bytes {
                continue;
            }
            used += text.len();
            let full_len = clipped.full_len;
            let unit = clipped.unit;
            row[col] = if text.encode_utf16().count() as u64 >= full_len {
                Value::Text(text)
            } else {
                Value::Clipped(Box::new(ora_core::ClippedValue {
                    preview: Value::Text(text),
                    full_len,
                    unit,
                }))
            };
        }
    }
}

/// Fill in the CLOB previews a data tab fetch left out, with one query per
/// [`PREVIEW_ROWIDS_PER_QUERY`] rows (not one trip per cell). Best effort:
/// when it fails the cells simply keep showing their length.
fn fill_previews(
    conn: &oracledb::Connection,
    dialect: &dyn ora_core::Dialect,
    owner: &str,
    table: &str,
    result: &mut QueryResult,
    limits: FetchLimits,
) {
    let Some(rowid_ix) = result
        .columns
        .iter()
        .position(|c| c.name == ora_core::ROWID_COLUMN)
    else {
        return;
    };
    let columns: Vec<usize> = (0..result.columns.len())
        .filter(|&ix| {
            ix != rowid_ix
                && result
                    .rows
                    .iter()
                    .any(|r| r.get(ix).is_some_and(is_unread_clob))
        })
        .collect();
    if columns.is_empty() {
        return;
    }
    let names: Vec<&str> = columns
        .iter()
        .map(|&ix| result.columns[ix].name.as_str())
        .collect();
    let ids: Vec<String> = result
        .rows
        .iter()
        .filter(|r| columns.iter().any(|&ix| is_unread_clob(&r[ix])))
        .filter_map(|r| match r.get(rowid_ix) {
            Some(Value::Text(id)) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut fetched: Vec<Vec<Value>> = Vec::new();
    for chunk in ids.chunks(PREVIEW_ROWIDS_PER_QUERY) {
        let sql = preview_sql(dialect, owner, table, &names, chunk.len());
        let plan = FetchPlan {
            max_previews: 0,
            label: "preview-fetch",
        };
        match fetch_rows(
            conn,
            &sql,
            chunk,
            FetchLimits::unlimited_bytes(chunk.len()),
            plan,
        ) {
            Ok(part) => fetched.extend(part.rows),
            Err(_) => return,
        }
    }
    merge_previews(result, rowid_ix, &columns, fetched, limits.max_result_bytes);
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
        self.in_read_scope(|conn| fetch_rows(conn, sql, binds, limits, FetchPlan::DEFAULT))
    }

    /// The data tab's page. CLOB cells are not read one by one while the
    /// cursor runs (a trip each, painfully slow over a VPN): the page comes
    /// back with their lengths and [`fill_previews`] then reads the starts
    /// of all of them in a few queries by ROWID.
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &ora_core::TableFilter,
        page: ora_core::TablePage,
    ) -> Result<QueryResult, DbError> {
        let dialect = self.dialect();
        let query = ora_core::filter::select_sql(dialect, owner, table, filter, page);
        let limits = FetchLimits::rows(page.limit);
        let plan = FetchPlan {
            max_previews: if page.rowid { 0 } else { MAX_LOB_PREVIEWS },
            label: "fetch",
        };
        self.in_read_scope(|conn| {
            let mut result = fetch_rows(conn, &query.sql, &query.binds, limits, plan)?;
            if page.rowid {
                fill_previews(conn, dialect, owner, table, &mut result, limits);
            }
            Ok(result)
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
                "SELECT owner, object_name, object_type FROM (\
                 SELECT o.owner, o.object_name, o.object_type FROM all_objects o \
                 WHERE o.owner = :1 AND o.object_type IN \
                 ('TABLE','VIEW','MATERIALIZED VIEW','INDEX','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TRIGGER','SYNONYM','TYPE') \
                 AND NOT (o.object_type = 'TYPE' AND o.object_name LIKE 'SYS\\_PLSQL\\_%' ESCAPE '\\') \
                 AND NOT (o.object_type = 'TABLE' AND EXISTS \
                   (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name)) \
                 UNION ALL \
                 SELECT l.owner, l.db_link, 'DATABASE LINK' FROM all_db_links l WHERE l.owner = :2) \
                 ORDER BY object_type, object_name",
                &[&owner_param, &owner_param],
            )
            .map_err(db_err)?;
        let mut objects = Vec::new();
        for row in cursor {
            let row = row.map_err(db_err)?;
            let owner: String = row.get(0).map_err(db_err)?;
            let name: String = row.get(1).map_err(db_err)?;
            let raw_type: String = row.get(2).map_err(db_err)?;
            objects.push(SchemaObject {
                owner,
                name,
                kind: ObjectKind::from_oracle_type(&raw_type),
            });
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
        let conn = self.conn()?;
        // End each statement with `;`/`/` and pretty-print, like Toad's
        // View Script. Session-scoped, so setting it every call is cheap and
        // never leaks into other sessions.
        conn.execute(
            "BEGIN \
             DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'SQLTERMINATOR', TRUE); \
             DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'PRETTY', TRUE); \
             END;",
            &[],
        )
        .map_err(db_err)?;
        // DBMS_METADATA spells multi-word types with an underscore
        // (MATERIALIZED_VIEW).
        let type_owned = match kind {
            ObjectKind::DbLink => "DB_LINK".to_string(),
            other => other.oracle_type().replace(' ', "_"),
        };
        let type_param: &str = &type_owned;
        let name_param: &str = name;
        let owner_param: &str = owner;
        let cursor = conn
            .query(
                "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
                &[&type_param, &name_param, &owner_param],
            )
            .map_err(db_err)?;
        let Some(row) = cursor.into_iter().next() else {
            return Err(DbError::new(format!("No DDL returned for {owner}.{name}")));
        };
        let ddl: Option<String> = row.map_err(db_err)?.get(0).map_err(db_err)?;
        let mut ddl = ddl.unwrap_or_default().trim().to_string();
        // GET_DDL('TYPE') is the spec only; add the body when there is one.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn clob(full_len: u64, preview: &str) -> Value {
        Value::Clipped(Box::new(ora_core::ClippedValue {
            preview: Value::Text(preview.to_string()),
            full_len,
            unit: ora_core::LenUnit::Chars,
        }))
    }

    fn page_result(rows: Vec<Vec<Value>>) -> QueryResult {
        let col = |name: &str| ColumnInfo {
            name: name.to_string(),
            data_type: "X".to_string(),
        };
        QueryResult {
            columns: vec![col(ora_core::ROWID_COLUMN), col("ID"), col("BODY")],
            rows,
            truncated: false,
            elapsed: Duration::ZERO,
            fetch_time: None,
            byte_limited: false,
        }
    }

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
    fn merged_previews_keep_clipped_semantics() {
        let mut result = page_result(vec![
            vec![text("R1"), Value::Integer(1), clob(5, "")],
            vec![text("R2"), Value::Integer(2), clob(5000, "")],
            vec![text("R3"), Value::Integer(3), clob(7, "")],
        ]);
        let rows = vec![
            vec![text("R2"), text("abc"), text("def")],
            vec![text("R1"), text("hello"), Value::Null],
        ];
        merge_previews(&mut result, 0, &[2], rows, usize::MAX);
        // Whole value fitted: a plain text cell.
        assert_eq!(result.rows[0][2], text("hello"));
        // Longer than the preview: still clipped, full length kept.
        assert_eq!(result.rows[1][2], clob(5000, "abcdef"));
        // Row the preview query did not return: untouched.
        assert_eq!(result.rows[2][2], clob(7, ""));
    }

    #[test]
    fn merged_previews_respect_the_result_byte_budget() {
        let mut result = page_result(vec![vec![text("R1"), Value::Integer(1), clob(9, "")]]);
        let rows = vec![vec![text("R1"), text("123456789"), Value::Null]];
        let used = result.heap_bytes();
        merge_previews(&mut result, 0, &[2], rows, used + 3);
        assert_eq!(result.rows[0][2], clob(9, ""));
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
}
