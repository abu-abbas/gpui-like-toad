//! Limits on how much one query may pull into memory, and the row
//! collector every driver fills while it walks a cursor.
//!
//! A row cap alone does not bound memory: 1,000 rows of 32 KiB text are
//! already 32 MiB, and one row of a BLOB can be gigabytes. So a fetch is
//! limited three ways: rows, bytes kept per cell (longer values keep only
//! their start, see [`crate::Value::Clipped`]), and bytes for the whole
//! result (the fetch stops early and reports `truncated`).

use std::time::{Duration, Instant};

use crate::model::{ColumnInfo, QueryResult, Value};
use crate::DEFAULT_MAX_ROWS;

/// Bytes kept per text/binary cell in the grid. The grid shows one line
/// and the Details panel a scrolling text, so 64 KiB is plenty to read;
/// "Load full value" fetches the rest on demand.
pub const MAX_CELL_BYTES: usize = 64 * 1024;

/// Bytes one grid result may hold before the fetch stops early.
pub const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;

/// Per-cell limit for "Export Data": exports need whole values, but one
/// cell still must not take the process down.
pub const EXPORT_MAX_CELL_BYTES: usize = 16 * 1024 * 1024;

/// Whole-result limit for "Export Data".
pub const EXPORT_MAX_RESULT_BYTES: usize = 512 * 1024 * 1024;

/// Largest value "Load full value" brings into the grid.
pub const MAX_FULL_CELL_BYTES: usize = 64 * 1024 * 1024;

/// "12 KiB", "3.5 MiB": a byte count for messages.
pub fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} bytes")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{:.1} MiB", b / (KIB * KIB))
    }
}

/// How much one fetch may keep in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchLimits {
    pub max_rows: usize,
    /// Text/binary cells longer than this keep only their start.
    pub max_cell_bytes: usize,
    /// The fetch stops (and sets `truncated` + `byte_limited`) once the
    /// kept rows reach this many bytes.
    pub max_result_bytes: usize,
    /// Oracle: read LOB values up to `max_cell_bytes` (exports, "Load full
    /// value"). Off for the grid, which reads only the start of the first
    /// few CLOBs and never a BLOB. Either way LOBs arrive as locators and
    /// reads stop at the limit, so a huge LOB is never pulled whole.
    pub read_lobs: bool,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self::grid()
    }
}

impl FetchLimits {
    /// The SQL editor's grid: [`DEFAULT_MAX_ROWS`] rows.
    pub const fn grid() -> Self {
        Self::rows(DEFAULT_MAX_ROWS)
    }

    /// Grid byte limits with a different row cap (data tab pages).
    pub const fn rows(max_rows: usize) -> Self {
        FetchLimits {
            max_rows,
            max_cell_bytes: MAX_CELL_BYTES,
            max_result_bytes: MAX_RESULT_BYTES,
            read_lobs: false,
        }
    }

    /// "Export Data" on a whole table.
    pub const fn export() -> Self {
        FetchLimits {
            max_rows: crate::EXPORT_MAX_ROWS,
            max_cell_bytes: EXPORT_MAX_CELL_BYTES,
            max_result_bytes: EXPORT_MAX_RESULT_BYTES,
            read_lobs: true,
        }
    }

    /// One cell fetched in full ("Load full value"). `budget` is what the
    /// open results can still take (see the app's all-tabs budget): a
    /// value that does not fit is not stored.
    pub const fn full_cell(budget: usize) -> Self {
        let cell = if budget < MAX_FULL_CELL_BYTES {
            budget
        } else {
            MAX_FULL_CELL_BYTES
        };
        FetchLimits {
            max_rows: 1,
            max_cell_bytes: cell,
            max_result_bytes: usize::MAX,
            read_lobs: true,
        }
    }

    /// No byte limits, only a row cap (internal catalog queries and tests).
    pub const fn unlimited_bytes(max_rows: usize) -> Self {
        FetchLimits {
            max_rows,
            max_cell_bytes: usize::MAX,
            max_result_bytes: usize::MAX,
            read_lobs: true,
        }
    }
}

/// Collects rows under [`FetchLimits`]. Drivers call [`Self::is_full`]
/// before converting the next cursor row, so a stopped fetch never
/// converts (or reads LOB data for) a row it would throw away.
pub struct RowCollector {
    limits: FetchLimits,
    rows: Vec<Vec<Value>>,
    bytes: usize,
    truncated: bool,
    byte_limited: bool,
    started: Instant,
    fetching: Option<Instant>,
}

impl RowCollector {
    pub fn new(limits: FetchLimits) -> Self {
        RowCollector {
            limits,
            rows: Vec::new(),
            bytes: 0,
            truncated: false,
            byte_limited: false,
            started: Instant::now(),
            fetching: None,
        }
    }

    /// Like [`Self::new`], with the statement's start time measured by the
    /// caller (before prepare/execute).
    pub fn started_at(limits: FetchLimits, started: Instant) -> Self {
        let mut collector = Self::new(limits);
        collector.started = started;
        collector
    }

    pub fn limits(&self) -> FetchLimits {
        self.limits
    }

    /// Mark the start of cursor consumption (for `fetch_time`).
    pub fn start_fetch(&mut self) {
        self.fetching = Some(Instant::now());
    }

    /// True when another row must not be kept. Call it only when a next
    /// row exists: it records that the result was cut short.
    pub fn is_full(&mut self) -> bool {
        if self.rows.len() >= self.limits.max_rows {
            self.truncated = true;
            return true;
        }
        if self.bytes >= self.limits.max_result_bytes {
            self.truncated = true;
            self.byte_limited = true;
            return true;
        }
        false
    }

    /// Bytes the result may still take before [`FetchLimits::max_result_bytes`].
    pub fn remaining_bytes(&self) -> usize {
        self.limits.max_result_bytes.saturating_sub(self.bytes)
    }

    /// Keep `row` if it fits in what is left of the result's byte budget.
    /// A row that does not fit is dropped whole (the result is marked
    /// `truncated` + `byte_limited`) and `false` is returned: the caller
    /// stops fetching. `is_full` alone cannot promise this, because it
    /// checks before the row's size is known.
    #[must_use]
    pub fn push(&mut self, row: Vec<Value>) -> bool {
        let size =
            std::mem::size_of::<Vec<Value>>() + row.iter().map(Value::heap_bytes).sum::<usize>();
        if size > self.remaining_bytes() {
            self.truncated = true;
            self.byte_limited = true;
            return false;
        }
        self.bytes += size;
        self.rows.push(row);
        true
    }

    /// Text cell under the per-cell limit.
    pub fn text(&self, text: &str) -> Value {
        Value::text_limited(text, self.limits.max_cell_bytes)
    }

    /// Binary cell under the per-cell limit.
    pub fn bytes(&self, bytes: &[u8]) -> Value {
        Value::bytes_limited(bytes, self.limits.max_cell_bytes)
    }

    pub fn finish(self, columns: Vec<ColumnInfo>) -> QueryResult {
        QueryResult {
            columns,
            rows: self.rows,
            truncated: self.truncated,
            byte_limited: self.byte_limited,
            elapsed: self.started.elapsed(),
            fetch_time: self.fetching.map(|f| f.elapsed()),
        }
    }

    /// For backends that build rows up front (demo): keep rows until a
    /// limit is hit.
    pub fn collect_all(
        limits: FetchLimits,
        columns: Vec<ColumnInfo>,
        rows: impl IntoIterator<Item = Vec<Value>>,
        elapsed: Duration,
    ) -> QueryResult {
        let mut collector = RowCollector::new(limits);
        for row in rows {
            if collector.is_full() {
                break;
            }
            let row = row.into_iter().map(|v| clip_owned(v, limits)).collect();
            if !collector.push(row) {
                break;
            }
        }
        let mut result = collector.finish(columns);
        result.elapsed = elapsed;
        result.fetch_time = None;
        result
    }
}

/// Apply the per-cell limit to an already built value.
pub fn clip_owned(value: Value, limits: FetchLimits) -> Value {
    match value {
        Value::Text(s) => Value::owned_text_limited(s, limits.max_cell_bytes),
        Value::Bytes(b) => Value::owned_bytes_limited(b, limits.max_cell_bytes),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LenUnit;

    fn col() -> Vec<ColumnInfo> {
        vec![ColumnInfo {
            name: "C".into(),
            data_type: "CLOB".into(),
        }]
    }

    #[test]
    fn long_text_keeps_only_its_start_at_a_char_boundary() {
        // "é" is two bytes: a 3-byte cap must not split the second one.
        let v = Value::text_limited("éé", 3);
        let Value::Clipped(c) = &v else {
            panic!("expected clipped, got {v:?}")
        };
        assert_eq!(c.preview, Value::Text("é".into()));
        assert_eq!(c.full_len, 4);
        assert_eq!(c.unit, LenUnit::Bytes);
        assert!(v.display().contains("4 bytes"));
        assert_eq!(Value::text_limited("ab", 2), Value::Text("ab".into()));
    }

    #[test]
    fn owned_clip_frees_the_tail() {
        let v = Value::owned_text_limited("x".repeat(10_000), 100);
        assert!(v.heap_bytes() < 400, "kept {} bytes", v.heap_bytes());
        let b = Value::owned_bytes_limited(vec![7; 10_000], 16);
        let Value::Clipped(c) = &b else { panic!() };
        assert_eq!(c.preview, Value::Bytes(vec![7; 16]));
        assert_eq!(c.full_len, 10_000);
    }

    #[test]
    fn result_byte_limit_stops_the_fetch_and_says_why() {
        let limits = FetchLimits {
            max_rows: 1000,
            max_cell_bytes: 1024,
            max_result_bytes: 10 * 1024,
            read_lobs: false,
        };
        let rows = (0..100).map(|_| vec![Value::Text("y".repeat(4096))]);
        let result = RowCollector::collect_all(limits, col(), rows, Duration::ZERO);
        // Each row keeps 1 KiB plus overhead: the 10 KiB budget is reached
        // after ten rows at most.
        assert!(result.row_count() <= 10, "{} rows", result.row_count());
        assert!(result.truncated && result.byte_limited);
        assert_eq!(result.clipped_cells(), result.row_count());
        assert!(result.heap_bytes() < 12 * 1024);
    }

    #[test]
    fn a_row_that_does_not_fit_is_not_stored() {
        let limits = FetchLimits {
            max_rows: 100,
            max_cell_bytes: usize::MAX,
            max_result_bytes: 1024,
            read_lobs: false,
        };
        // The first row alone is bigger than the whole budget.
        let rows = vec![vec![Value::Text("z".repeat(4096))]];
        let result = RowCollector::collect_all(limits, col(), rows, Duration::ZERO);
        assert_eq!(result.row_count(), 0);
        assert!(result.truncated && result.byte_limited);
        assert!(result.heap_bytes() <= 1024);
    }

    #[test]
    fn rows_stop_at_the_exact_budget_never_over() {
        let limits = FetchLimits {
            max_rows: 100,
            max_cell_bytes: usize::MAX,
            max_result_bytes: 5000,
            read_lobs: false,
        };
        let rows = (0..50).map(|_| vec![Value::Text("q".repeat(1000))]);
        let result = RowCollector::collect_all(limits, col(), rows, Duration::ZERO);
        assert!(result.row_count() >= 1 && result.row_count() < 50);
        assert!(result.heap_bytes() <= 5000, "{}", result.heap_bytes());
        assert!(result.byte_limited);
    }

    #[test]
    fn full_cell_limit_follows_the_budget_but_never_exceeds_the_cap() {
        assert_eq!(FetchLimits::full_cell(1000).max_cell_bytes, 1000);
        assert_eq!(FetchLimits::full_cell(0).max_cell_bytes, 0);
        assert_eq!(
            FetchLimits::full_cell(usize::MAX).max_cell_bytes,
            MAX_FULL_CELL_BYTES
        );
    }

    #[test]
    fn small_results_are_untouched() {
        let rows = (0..3).map(|i| vec![Value::Integer(i)]);
        let result = RowCollector::collect_all(FetchLimits::grid(), col(), rows, Duration::ZERO);
        assert_eq!(result.row_count(), 3);
        assert!(!result.truncated && !result.byte_limited);
        assert_eq!(result.clipped_cells(), 0);
    }

    #[test]
    fn row_cap_is_not_reported_as_a_byte_limit() {
        let rows = (0..5).map(|i| vec![Value::Integer(i)]);
        let result = RowCollector::collect_all(FetchLimits::rows(2), col(), rows, Duration::ZERO);
        assert_eq!(result.row_count(), 2);
        assert!(result.truncated && !result.byte_limited);
    }
}
