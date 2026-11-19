use crate::model::{QueryResult, Value};

/// Render a [`QueryResult`] as an `.xlsx` workbook (single sheet named
/// "Results"): bold header row, numbers as numeric cells, text as string
/// cells, NULL as blank cells. Returns the raw workbook bytes; the caller
/// writes them to a file.
///
/// `Value::Integer` is i128 while Excel stores IEEE-754 doubles, so very
/// large integers lose precision past 2^53 — acceptable for a grid export,
/// and the CSV export keeps the exact text.
pub fn to_xlsx(result: &QueryResult) -> Result<Vec<u8>, String> {
    write_xlsx(result, &mut |_| true).map_err(|e| e.to_string())
}

/// Why a background export stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// The progress callback returned `false`.
    Cancelled,
    /// Building or writing the file failed.
    Failed(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Cancelled => f.write_str("Export cancelled"),
            ExportError::Failed(message) => f.write_str(message),
        }
    }
}

/// Rows between two progress callbacks.
const PROGRESS_EVERY: usize = 1000;

/// [`to_xlsx`] with progress: `progress(rows_done)` runs every
/// [`PROGRESS_EVERY`] rows and returning `false` cancels the export.
pub fn write_xlsx(
    result: &QueryResult,
    progress: &mut dyn FnMut(usize) -> bool,
) -> Result<Vec<u8>, ExportError> {
    use rust_xlsxwriter::{Format, Workbook};
    let failed = |e: rust_xlsxwriter::XlsxError| ExportError::Failed(e.to_string());

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Results").map_err(failed)?;
    let header_format = Format::new().set_bold();
    for (col, column) in result.columns.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, column.name.as_str(), &header_format)
            .map_err(failed)?;
    }
    for (row_ix, row) in result.rows.iter().enumerate() {
        if row_ix % PROGRESS_EVERY == 0 && !progress(row_ix) {
            return Err(ExportError::Cancelled);
        }
        let row_num = (row_ix + 1) as u32;
        for (col_ix, value) in row.iter().enumerate() {
            let col_num = col_ix as u16;
            // `write_string` always stores a text cell, so a value such as
            // "=1+1" stays text in XLSX (no formula injection).
            match value {
                Value::Null => {}
                Value::Integer(n) => {
                    sheet
                        .write_number(row_num, col_num, *n as f64)
                        .map_err(failed)?;
                }
                Value::Float(f) => {
                    sheet.write_number(row_num, col_num, *f).map_err(failed)?;
                }
                Value::Text(_) | Value::Bytes(_) | Value::Clipped(_) => {
                    sheet
                        .write_string(row_num, col_num, value.display())
                        .map_err(failed)?;
                }
            }
        }
    }
    if !progress(result.rows.len()) {
        return Err(ExportError::Cancelled);
    }
    workbook.save_to_buffer().map_err(failed)
}

/// How text cells are written to CSV.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CsvMode {
    /// Exact values, for data interchange and re-import.
    #[default]
    Raw,
    /// For opening in Excel or LibreOffice: a text cell (or header) that
    /// starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a
    /// leading `'`, so the spreadsheet shows text instead of running it as
    /// a formula. Numbers are written as they are. The apostrophe is part
    /// of the file, so use [`CsvMode::Raw`] when the file is read by code.
    SpreadsheetSafe,
}

/// Prefix that makes a spreadsheet treat a cell as text.
const TEXT_PREFIX: char = '\'';

/// `value` as one safe CSV cell text (before CSV quoting).
fn spreadsheet_safe(value: &str) -> std::borrow::Cow<'_, str> {
    match value.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => {
            let mut out = String::with_capacity(value.len() + 1);
            out.push(TEXT_PREFIX);
            out.push_str(value);
            std::borrow::Cow::Owned(out)
        }
        _ => std::borrow::Cow::Borrowed(value),
    }
}

fn cell_text(value: &Value, mode: CsvMode) -> String {
    let text = value.display();
    match (mode, value) {
        (CsvMode::SpreadsheetSafe, Value::Text(_) | Value::Bytes(_) | Value::Clipped(_)) => {
            spreadsheet_safe(&text).into_owned()
        }
        _ => text,
    }
}

/// Render a [`QueryResult`] as RFC 4180 CSV: CRLF line endings, fields
/// quoted when they contain a comma, quote, or line break, embedded quotes
/// doubled. NULL renders as an empty field (same as the grid).
pub fn to_csv(result: &QueryResult) -> String {
    to_csv_mode(result, CsvMode::Raw)
}

/// [`to_csv`] with a [`CsvMode`].
pub fn to_csv_mode(result: &QueryResult, mode: CsvMode) -> String {
    let mut out = Vec::new();
    // Writing to a Vec cannot fail and the callback never cancels.
    let _ = write_csv(result, mode, &mut out, &mut |_| true);
    String::from_utf8(out).unwrap_or_default()
}

/// Stream a [`QueryResult`] as CSV to `out`, one record at a time (nothing
/// but the current record is built in memory). `progress(rows_done)` runs
/// every [`PROGRESS_EVERY`] rows and returning `false` cancels.
pub fn write_csv<W: std::io::Write>(
    result: &QueryResult,
    mode: CsvMode,
    out: &mut W,
    progress: &mut dyn FnMut(usize) -> bool,
) -> Result<(), ExportError> {
    let io = |e: std::io::Error| ExportError::Failed(e.to_string());
    let header: Vec<String> = result
        .columns
        .iter()
        .map(|c| match mode {
            CsvMode::Raw => c.name.clone(),
            CsvMode::SpreadsheetSafe => spreadsheet_safe(&c.name).into_owned(),
        })
        .collect();
    write_record(out, header.iter().map(String::as_str)).map_err(io)?;
    for (ix, row) in result.rows.iter().enumerate() {
        if ix % PROGRESS_EVERY == 0 && !progress(ix) {
            return Err(ExportError::Cancelled);
        }
        let cells: Vec<String> = row.iter().map(|v| cell_text(v, mode)).collect();
        write_record(out, cells.iter().map(String::as_str)).map_err(io)?;
    }
    if !progress(result.rows.len()) {
        return Err(ExportError::Cancelled);
    }
    Ok(())
}

/// Write an export to `path` without ever leaving a half-written file
/// there: the bytes go to a sibling `<name>.part` file that replaces `path`
/// only when `fill` succeeds; on cancel or error it is removed.
pub fn write_file_atomically<F>(path: &std::path::Path, fill: F) -> Result<(), ExportError>
where
    F: FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), ExportError>,
{
    use std::io::Write;
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let failed =
        |e: std::io::Error| ExportError::Failed(format!("Failed to write {}: {e}", path.display()));
    let file = std::fs::File::create(&part).map_err(failed)?;
    let mut writer = std::io::BufWriter::new(file);
    let outcome = fill(&mut writer).and_then(|()| writer.flush().map_err(failed));
    drop(writer);
    match outcome {
        Ok(()) => std::fs::rename(&part, path).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            failed(e)
        }),
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            Err(err)
        }
    }
}

/// [`write_csv`] to a file, atomically (see [`write_file_atomically`]).
pub fn write_csv_file(
    result: &QueryResult,
    mode: CsvMode,
    path: &std::path::Path,
    progress: &mut dyn FnMut(usize) -> bool,
) -> Result<(), ExportError> {
    write_file_atomically(path, |out| write_csv(result, mode, out, progress))
}

/// [`write_xlsx`] to a file, atomically.
pub fn write_xlsx_file(
    result: &QueryResult,
    path: &std::path::Path,
    progress: &mut dyn FnMut(usize) -> bool,
) -> Result<(), ExportError> {
    use std::io::Write;
    let bytes = write_xlsx(result, progress)?;
    write_file_atomically(path, |out| {
        out.write_all(&bytes)
            .map_err(|e| ExportError::Failed(e.to_string()))
    })
}

fn write_record<'a, W: std::io::Write>(
    out: &mut W,
    fields: impl Iterator<Item = &'a str>,
) -> std::io::Result<()> {
    for (i, field) in fields.enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        out.write_all(escape(field).as_bytes())?;
    }
    out.write_all(b"\r\n")
}

fn escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\r') || field.contains('\n') {
        let mut out = String::with_capacity(field.len() + 2);
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnInfo, Value};

    fn sample() -> QueryResult {
        QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "ID".into(),
                    data_type: "NUMBER".into(),
                },
                ColumnInfo {
                    name: "NAME".into(),
                    data_type: "VARCHAR2(20)".into(),
                },
                ColumnInfo {
                    name: "NOTE".into(),
                    data_type: "VARCHAR2(50)".into(),
                },
            ],
            rows: vec![
                vec![
                    Value::Integer(1),
                    Value::Text("Ada".into()),
                    Value::Text("says \"hi\", then\nleft".into()),
                ],
                vec![Value::Integer(2), Value::Null, Value::Text("plain".into())],
            ],
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
        }
    }

    #[test]
    fn csv_quotes_and_crlf() {
        let csv = to_csv(&sample());
        assert_eq!(
            csv,
            "ID,NAME,NOTE\r\n1,Ada,\"says \"\"hi\"\", then\nleft\"\r\n2,,plain\r\n"
        );
    }

    #[test]
    fn csv_empty_result_writes_header_only() {
        let empty = QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "A".into(),
                    data_type: "NUMBER".into(),
                },
                ColumnInfo {
                    name: "B".into(),
                    data_type: "VARCHAR2".into(),
                },
            ],
            rows: vec![],
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
        };
        assert_eq!(to_csv(&empty), "A,B\r\n");
        assert_eq!(to_csv(&QueryResult::default()), "\r\n");
    }

    #[test]
    fn csv_unicode_unquoted_and_cr_quoted() {
        let result = QueryResult {
            columns: vec![ColumnInfo {
                name: "T".into(),
                data_type: "VARCHAR2".into(),
            }],
            rows: vec![
                vec![Value::Text("café ☕".into())],
                vec![Value::Text("a\rb".into())],
            ],
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
        };
        assert_eq!(to_csv(&result), "T\r\ncafé ☕\r\n\"a\rb\"\r\n");
    }

    #[test]
    fn xlsx_writes_a_valid_zip_workbook() {
        let bytes = to_xlsx(&sample()).unwrap();
        // ZIP magic: PK\x03\x04.
        assert!(bytes.len() > 100, "workbook should be non-trivial");
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
        // A workbook with different data differs.
        let other = to_xlsx(&QueryResult::default()).unwrap();
        assert_ne!(bytes, other);
        assert_eq!(&other[0..4], &[0x50, 0x4B, 0x03, 0x04]);
    }

    #[test]
    fn xlsx_round_trips_through_a_file() {
        let bytes = to_xlsx(&sample()).unwrap();
        // Test-only scratch file, unique per process id; no secrets involved.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let path = std::env::temp_dir().join(format!(
            "gpui-like-toad-xlsx-test-{}.xlsx",
            std::process::id()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let read_back = std::fs::read(&path).unwrap();
        assert_eq!(read_back.len(), bytes.len());
        assert!(!read_back.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn xlsx_handles_floats_bytes_and_nulls() {
        let result = QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "F".into(),
                    data_type: "BINARY_DOUBLE".into(),
                },
                ColumnInfo {
                    name: "B".into(),
                    data_type: "RAW".into(),
                },
                ColumnInfo {
                    name: "N".into(),
                    data_type: "VARCHAR2".into(),
                },
            ],
            rows: vec![vec![
                Value::Float(1.5),
                Value::Bytes(vec![0xDE, 0xAD]),
                Value::Null,
            ]],
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
        };
        let bytes = to_xlsx(&result).unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
    }

    fn one_column(name: &str, values: Vec<Value>) -> QueryResult {
        QueryResult {
            columns: vec![ColumnInfo {
                name: name.into(),
                data_type: "VARCHAR2".into(),
            }],
            rows: values.into_iter().map(|v| vec![v]).collect(),
            ..QueryResult::default()
        }
    }

    #[test]
    fn safe_mode_keeps_formula_like_text_as_text() {
        let result = one_column(
            "=HEADER()",
            vec![
                Value::Text("=1+1".into()),
                Value::Text("+cmd".into()),
                Value::Text("-2+3".into()),
                Value::Text("@SUM(A1)".into()),
                Value::Text("\tTAB".into()),
                Value::Text("\rCR".into()),
                Value::Text("normal = fine".into()),
                Value::Text(String::new()),
                Value::Null,
                // Real numbers stay numbers, even negative ones.
                Value::Integer(-5),
                Value::Float(-1.5),
            ],
        );
        assert_eq!(
            to_csv_mode(&result, CsvMode::SpreadsheetSafe),
            "'=HEADER()\r\n'=1+1\r\n'+cmd\r\n'-2+3\r\n'@SUM(A1)\r\n'\tTAB\r\n\"'\rCR\"\r\nnormal = fine\r\n\r\n\r\n-5\r\n-1.5\r\n"
        );
    }

    #[test]
    fn raw_mode_is_untouched() {
        let result = one_column("A", vec![Value::Text("=1+1".into())]);
        assert_eq!(to_csv_mode(&result, CsvMode::Raw), "A\r\n=1+1\r\n");
        assert_eq!(to_csv(&result), to_csv_mode(&result, CsvMode::Raw));
    }

    #[test]
    fn csv_stream_reports_progress_and_cancels() {
        let rows = (0..2500).map(Value::Integer).collect();
        let result = one_column("N", rows);
        let mut seen = Vec::new();
        let mut out = Vec::new();
        write_csv(&result, CsvMode::Raw, &mut out, &mut |n| {
            seen.push(n);
            true
        })
        .unwrap();
        assert_eq!(seen, [0, 1000, 2000, 2500]);
        let mut out = Vec::new();
        let err = write_csv(&result, CsvMode::Raw, &mut out, &mut |n| n < 1000).unwrap_err();
        assert_eq!(err, ExportError::Cancelled);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        // Test-only scratch directory, unique per process; no secrets involved.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!(
            "gpui-like-toad-export-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_export_is_atomic_and_cancel_leaves_nothing() {
        let dir = scratch("atomic");
        let path = dir.join("out.csv");
        std::fs::write(&path, "old").unwrap();
        let result = one_column("N", (0..2500).map(Value::Integer).collect());
        // Cancelled: the old file stays, no .part file is left.
        let err = write_csv_file(&result, CsvMode::Raw, &path, &mut |n| n < 1000).unwrap_err();
        assert_eq!(err, ExportError::Cancelled);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert!(!dir.join("out.csv.part").exists());
        // Completed: the file is replaced.
        let mut last = 0;
        write_csv_file(&result, CsvMode::Raw, &path, &mut |n| {
            last = n;
            true
        })
        .unwrap();
        assert_eq!(last, 2500);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("N\r\n0\r\n1\r\n") && text.ends_with("2499\r\n"));
        assert!(!dir.join("out.csv.part").exists());
        // An unwritable target reports the path and leaves nothing behind.
        let bad = dir.join("missing-dir").join("x.csv");
        let err = write_csv_file(&result, CsvMode::Raw, &bad, &mut |_| true).unwrap_err();
        assert!(matches!(err, ExportError::Failed(m) if m.contains("x.csv")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xlsx_file_export_cancels_and_writes_on_success() {
        let dir = scratch("xlsx");
        let path = dir.join("out.xlsx");
        let result = one_column("A", vec![Value::Text("=1+1".into()); 2500]);
        let err = write_xlsx_file(&result, &path, &mut |n| n < 1000).unwrap_err();
        assert_eq!(err, ExportError::Cancelled);
        assert!(!path.exists());
        write_xlsx_file(&result, &path, &mut |_| true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
