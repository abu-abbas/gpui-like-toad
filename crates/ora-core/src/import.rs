//! CSV import: parse a CSV file and turn its rows into `INSERT` statements
//! for one table.
//!
//! The parser follows RFC 4180 (quoted fields, doubled quotes, CRLF or LF
//! line endings, newlines inside quotes) and strips a UTF-8 BOM, so files
//! written by [`crate::export::to_csv`] and by Excel round-trip. The first
//! record is the header; header names are matched to table columns
//! case-insensitively.

use crate::model::ColumnDef;

/// A parsed CSV file: header names plus data rows. Every row has exactly
/// `headers.len()` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Longest single line or record (a quoted field may span lines) the
/// reader accepts. A file with an unclosed quote or no line breaks fails
/// with a message instead of being read into memory whole.
pub const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// One record with the line it starts on.
type Record = (usize, Vec<String>);

/// Incremental RFC 4180 record splitter: feed it text in pieces (whole
/// physical lines, or a whole string) and it hands back finished records.
struct RecordParser {
    delimiter: char,
    fields: Vec<String>,
    field: String,
    in_quotes: bool,
    /// Last character fed was a closing quote (an empty quoted last field).
    ended_with_quote: bool,
    line: usize,
    record_line: usize,
    record_bytes: usize,
}

impl RecordParser {
    fn new(delimiter: char) -> Self {
        RecordParser {
            delimiter,
            fields: Vec::new(),
            field: String::new(),
            in_quotes: false,
            ended_with_quote: false,
            line: 1,
            record_line: 1,
            record_bytes: 0,
        }
    }

    /// Feed `text`; finished records are appended to `out`. A piece must not
    /// split a `\r\n` pair.
    fn feed(&mut self, text: &str, out: &mut Vec<Record>) -> Result<(), String> {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            self.ended_with_quote = false;
            self.record_bytes += c.len_utf8();
            if self.record_bytes > MAX_RECORD_BYTES {
                return Err(format!(
                    "Line {}: the record is larger than {} MiB (is a quote left open?)",
                    self.record_line,
                    MAX_RECORD_BYTES / (1024 * 1024)
                ));
            }
            if self.in_quotes {
                match c {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        self.field.push('"');
                    }
                    '"' => {
                        self.in_quotes = false;
                        self.ended_with_quote = chars.peek().is_none();
                    }
                    '\n' => {
                        self.line += 1;
                        self.field.push(c);
                    }
                    _ => self.field.push(c),
                }
                continue;
            }
            match c {
                '"' if self.field.is_empty() => self.in_quotes = true,
                c if c == self.delimiter => self.fields.push(std::mem::take(&mut self.field)),
                '\r' if chars.peek() == Some(&'\n') => {}
                '\n' => {
                    self.fields.push(std::mem::take(&mut self.field));
                    out.push((self.record_line, std::mem::take(&mut self.fields)));
                    self.line += 1;
                    self.record_line = self.line;
                    self.record_bytes = 0;
                }
                _ => self.field.push(c),
            }
        }
        Ok(())
    }

    /// End of input: an unclosed quote is an error, an unterminated last
    /// record is returned.
    fn finish(&mut self, out: &mut Vec<Record>) -> Result<(), String> {
        if self.in_quotes {
            return Err(format!("Line {}: unclosed quote", self.record_line));
        }
        if !self.field.is_empty() || !self.fields.is_empty() || self.ended_with_quote {
            self.fields.push(std::mem::take(&mut self.field));
            out.push((self.record_line, std::mem::take(&mut self.fields)));
        }
        Ok(())
    }
}

/// Streaming CSV reader: reads one physical line at a time, so memory use is
/// one record (at most [`MAX_RECORD_BYTES`]), not the file. Strips a UTF-8
/// BOM and accepts CRLF or LF.
pub struct CsvReader<R: std::io::BufRead> {
    reader: R,
    parser: RecordParser,
    ready: std::collections::VecDeque<Record>,
    line_buf: Vec<u8>,
    started: bool,
    eof: bool,
    headers: Option<Vec<String>>,
}

impl<R: std::io::BufRead> CsvReader<R> {
    pub fn new(reader: R) -> Self {
        CsvReader {
            reader,
            parser: RecordParser::new(','),
            ready: Default::default(),
            line_buf: Vec::new(),
            started: false,
            eof: false,
            headers: None,
        }
    }

    /// Next raw record (blank lines included), or `None` at the end.
    fn next_record(&mut self) -> Result<Option<Record>, String> {
        use std::io::{BufRead, Read};
        let mut out = Vec::new();
        while self.ready.is_empty() && !self.eof {
            self.line_buf.clear();
            let read = (&mut self.reader)
                .take(MAX_RECORD_BYTES as u64 + 1)
                .read_until(b'\n', &mut self.line_buf)
                .map_err(|e| format!("Failed to read the CSV file: {e}"))?;
            if read == 0 {
                self.eof = true;
                self.parser.finish(&mut out)?;
            } else {
                if self.line_buf.len() > MAX_RECORD_BYTES {
                    return Err(format!(
                        "Line {}: the line is larger than {} MiB",
                        self.parser.line,
                        MAX_RECORD_BYTES / (1024 * 1024)
                    ));
                }
                let mut text = std::str::from_utf8(&self.line_buf).map_err(|_| {
                    format!(
                        "Line {}: the file is not valid UTF-8 text",
                        self.parser.line
                    )
                })?;
                if !self.started {
                    self.started = true;
                    text = text.strip_prefix('\u{feff}').unwrap_or(text);
                }
                self.parser.feed(text, &mut out)?;
            }
            self.ready.extend(out.drain(..));
        }
        Ok(self.ready.pop_front())
    }

    /// Read and check the header record. Call once, before [`Self::next_row`].
    pub fn read_header(&mut self) -> Result<&[String], String> {
        let Some((_, headers)) = self.next_record()? else {
            return Err("The CSV file is empty".to_string());
        };
        let headers: Vec<String> = headers.into_iter().map(|h| h.trim().to_string()).collect();
        if headers.iter().all(|h| h.is_empty()) {
            return Err("The CSV header is empty".to_string());
        }
        Ok(self.headers.insert(headers))
    }

    /// Next data row with its 1-based line number, or `None` at the end.
    /// Skips blank lines; fails when the field count differs from the header.
    pub fn next_row(&mut self) -> Result<Option<Record>, String> {
        let width = self
            .headers
            .as_ref()
            .map(Vec::len)
            .ok_or("The CSV header was not read")?;
        while let Some((line, fields)) = self.next_record()? {
            // A blank line is not a record.
            if fields.len() == 1 && fields[0].is_empty() {
                continue;
            }
            if fields.len() != width {
                return Err(format!(
                    "Line {line}: found {} columns, but the header has {width}",
                    fields.len()
                ));
            }
            return Ok(Some((line, fields)));
        }
        Ok(None)
    }
}

/// Parse CSV text. Fails on an empty file, an unterminated quote, or a row
/// whose field count differs from the header (the message names the line).
/// Holds the whole file; imports stream through [`CsvReader`] instead.
pub fn parse_csv(text: &str) -> Result<CsvData, String> {
    let mut reader = CsvReader::new(text.as_bytes());
    let headers = reader.read_header()?.to_vec();
    let mut rows = Vec::new();
    while let Some((_, fields)) = reader.next_row()? {
        rows.push(fields);
    }
    Ok(CsvData { headers, rows })
}

/// Rows per `INSERT` batch of a streamed import, and the text bytes a batch
/// may hold (whichever is reached first closes the batch).
pub const IMPORT_BATCH_ROWS: usize = 500;
pub const IMPORT_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Read the data rows of a CSV stream in bounded batches (the header must
/// already be read) and hand each to `insert`, which returns the rows it
/// inserted. `insert` also gets the 1-based data-row number the batch
/// starts at. Returns the total inserted.
pub fn for_each_batch<R, F>(reader: &mut CsvReader<R>, mut insert: F) -> Result<u64, String>
where
    R: std::io::BufRead,
    F: FnMut(&[Vec<String>], u64) -> Result<u64, String>,
{
    let mut batch: Vec<Vec<String>> = Vec::new();
    let mut bytes = 0usize;
    let mut first_row = 1u64;
    let mut total = 0u64;
    loop {
        let row = reader.next_row()?;
        let last = row.is_none();
        if let Some((_, fields)) = row {
            bytes += fields.iter().map(String::len).sum::<usize>();
            batch.push(fields);
        }
        if !batch.is_empty()
            && (last || batch.len() >= IMPORT_BATCH_ROWS || bytes >= IMPORT_BATCH_BYTES)
        {
            total += insert(&batch, first_row)?;
            first_row += batch.len() as u64;
            batch.clear();
            bytes = 0;
        }
        if last {
            return Ok(total);
        }
    }
}

/// Read spreadsheet clipboard rows, preserving quoted tabs and line breaks.
pub fn clipboard_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut parser = RecordParser::new('\t');
    let mut records = Vec::new();
    parser.feed(text, &mut records)?;
    parser.finish(&mut records)?;
    Ok(records.into_iter().map(|(_, fields)| fields).collect())
}

/// Map each CSV header to the table column with the same name (ignoring
/// case). Returns the column names in CSV order, or an error that lists
/// every header the table does not have.
pub fn map_columns(headers: &[String], columns: &[ColumnDef]) -> Result<Vec<String>, String> {
    let mut mapped = Vec::with_capacity(headers.len());
    let mut unknown = Vec::new();
    for header in headers {
        match columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(header.trim()))
        {
            Some(col) => mapped.push(col.name.clone()),
            None => unknown.push(header.clone()),
        }
    }
    if unknown.is_empty() {
        Ok(mapped)
    } else {
        Err(format!(
            "CSV columns missing from the table: {}",
            unknown.join(", ")
        ))
    }
}

/// Quote an Oracle identifier, doubling embedded quotes.
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `OWNER.NAME` for DBMS_MVIEW calls: plain upper-case identifiers stay
/// unquoted (the form every Oracle version accepts), anything else is
/// quoted to keep its exact case.
pub fn mview_list_name(owner: &str, name: &str) -> String {
    let plain = |ident: &str| {
        !ident.is_empty()
            && ident.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && ident.chars().all(|c| {
                c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$' | '#')
            })
    };
    let part = |ident: &str| {
        if plain(ident) {
            ident.to_string()
        } else {
            quote_ident(ident)
        }
    };
    format!("{}.{}", part(owner), part(name))
}

/// The statement that refreshes one materialized view; bind `:1` is
/// [`mview_list_name`]. With no method given Oracle uses the refresh
/// method the view was defined with.
pub const REFRESH_MVIEW_SQL: &str = "BEGIN DBMS_MVIEW.REFRESH(:1); END;";

/// Quote a string literal, doubling embedded single quotes.
fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// One `INSERT` per row. Empty fields become `NULL` (Oracle treats `''` as
/// NULL anyway); everything else is a string literal and relies on Oracle's
/// implicit conversion for NUMBER/DATE columns.
pub fn insert_statement(owner: &str, table: &str, columns: &[String], row: &[String]) -> String {
    let cols = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let values = row
        .iter()
        .map(|v| {
            if v.is_empty() {
                "NULL".to_string()
            } else {
                quote_literal(v)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {}.{} ({cols}) VALUES ({values})",
        quote_ident(owner),
        quote_ident(table)
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn mview_names_are_quoted_only_when_needed() {
        assert_eq!(mview_list_name("HR", "MV_PEGAWAI"), "HR.MV_PEGAWAI");
        assert_eq!(mview_list_name("hr", "Mv One"), "\"hr\".\"Mv One\"");
        assert_eq!(mview_list_name("A1", "9X"), "A1.\"9X\"");
    }

    use super::*;

    #[test]
    fn reads_clipboard_without_a_header_and_preserves_quoted_fields() {
        let rows = clipboard_rows("1\t\"two\tcolumns\"\r\n2\t\"line\n\"\"two\"\"\"\r\n").unwrap();
        assert_eq!(rows[0], ["1", "two\tcolumns"]);
        assert_eq!(rows[1], ["2", "line\n\"two\""]);
        assert!(clipboard_rows("\"unfinished").is_err());
        assert_eq!(clipboard_rows("\"\"").unwrap()[0], [""]);
    }

    fn col(name: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: "VARCHAR2(20)".into(),
            nullable: true,
            default: None,
        }
    }

    #[test]
    fn parses_quotes_crlf_bom_and_embedded_newlines() {
        let text = "\u{feff}ID,NAME\r\n1,\"Doe, John\"\r\n2,\"say \"\"hi\"\"\nthere\"\r\n";
        let csv = parse_csv(text).unwrap();
        assert_eq!(csv.headers, ["ID", "NAME"]);
        assert_eq!(csv.rows.len(), 2);
        assert_eq!(csv.rows[0], ["1", "Doe, John"]);
        assert_eq!(csv.rows[1], ["2", "say \"hi\"\nthere"]);
    }

    #[test]
    fn round_trips_export_output() {
        let result = crate::model::QueryResult {
            columns: vec![crate::model::ColumnInfo {
                name: "A".into(),
                data_type: "VARCHAR2".into(),
            }],
            rows: vec![vec![crate::model::Value::Text("x,\"y\"".into())]],
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
            order_by: Vec::new(),
        };
        let csv = parse_csv(&crate::export::to_csv(&result)).unwrap();
        assert_eq!(csv.rows, [["x,\"y\""]]);
    }

    #[test]
    fn rejects_ragged_rows_and_open_quotes() {
        assert!(parse_csv("A,B\n1\n").unwrap_err().contains("Line 2"));
        assert!(parse_csv("A\n\"open\n").is_err());
        assert!(parse_csv("").is_err());
    }

    #[test]
    fn maps_headers_case_insensitively_and_reports_unknown() {
        let cols = [col("ID"), col("NAME")];
        assert_eq!(
            map_columns(&["name".into(), "Id".into()], &cols).unwrap(),
            ["NAME", "ID"]
        );
        let err = map_columns(&["ID".into(), "AGE".into()], &cols).unwrap_err();
        assert!(err.contains("AGE"));
    }

    #[test]
    fn insert_quotes_identifiers_literals_and_nulls() {
        let sql = insert_statement(
            "HR",
            "EMP",
            &["ID".into(), "NAME".into(), "NOTE".into()],
            &["1".into(), "O'Brien".into(), String::new()],
        );
        assert_eq!(
            sql,
            "INSERT INTO \"HR\".\"EMP\" (\"ID\", \"NAME\", \"NOTE\") VALUES ('1', 'O''Brien', NULL)"
        );
    }

    fn many_rows(n: usize) -> String {
        let mut text = String::from("ID,NOTE\n");
        for i in 0..n {
            text.push_str(&format!("{i},\"row {i}\nline two\"\n"));
        }
        text
    }

    #[test]
    fn reader_streams_across_tiny_buffer_chunks() {
        let text = format!("\u{feff}{}", many_rows(1200));
        // A 7-byte buffer forces every fill to split lines and characters.
        let reader = std::io::BufReader::with_capacity(7, text.as_bytes());
        let mut reader = CsvReader::new(reader);
        assert_eq!(reader.read_header().unwrap(), ["ID", "NOTE"]);
        let mut count = 0;
        while let Some((_, row)) = reader.next_row().unwrap() {
            assert_eq!(row[0], count.to_string());
            assert_eq!(row[1], format!("row {count}\nline two"));
            count += 1;
        }
        assert_eq!(count, 1200);
        assert_eq!(parse_csv(&text).unwrap().rows.len(), 1200);
    }

    #[test]
    fn reader_matches_old_edge_cases() {
        // Last record without a newline, trailing blank lines, empty quoted tail.
        assert_eq!(parse_csv("A,B\n1,2").unwrap().rows, [["1", "2"]]);
        assert_eq!(parse_csv("A\n1\n\n\n").unwrap().rows, [["1"]]);
        assert_eq!(parse_csv("A,B\n1,\"\"").unwrap().rows, [["1", ""]]);
        assert_eq!(parse_csv("A\r\n1\r\n").unwrap().rows, [["1"]]);
        assert!(parse_csv("A,B\n1,2\n3\n").unwrap_err().contains("Line 3"));
        assert!(parse_csv("A\n\"x").unwrap_err().contains("unclosed quote"));
        let err = CsvReader::new(&b"A\n\xff\n"[..]);
        let mut err = err;
        err.read_header().unwrap();
        assert!(err.next_row().unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn open_quote_does_not_read_the_whole_file() {
        // 20 MiB of lines inside one never-closed quote: refused at 16 MiB.
        struct Endless(usize);
        impl std::io::Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let head = b"A\n\"";
                let mut n = 0;
                while n < buf.len() && self.0 < 20 * 1024 * 1024 {
                    buf[n] = if self.0 < head.len() {
                        head[self.0]
                    } else if self.0 % 64 == 63 {
                        b'\n'
                    } else {
                        b'x'
                    };
                    self.0 += 1;
                    n += 1;
                }
                Ok(n)
            }
        }
        let mut reader = CsvReader::new(std::io::BufReader::new(Endless(0)));
        reader.read_header().unwrap();
        let err = reader.next_row().unwrap_err();
        assert!(err.contains("larger than 16 MiB"), "{err}");
    }

    #[test]
    fn batches_are_bounded_and_numbered() {
        let text = many_rows(1201);
        let mut reader = CsvReader::new(text.as_bytes());
        reader.read_header().unwrap();
        let mut sizes = Vec::new();
        let mut starts = Vec::new();
        let total = for_each_batch(&mut reader, |rows, first| {
            sizes.push(rows.len());
            starts.push(first);
            Ok(rows.len() as u64)
        })
        .unwrap();
        assert_eq!(total, 1201);
        assert_eq!(sizes, [500, 500, 201]);
        assert_eq!(starts, [1, 501, 1001]);
        // A failing batch stops the import.
        let mut reader = CsvReader::new(text.as_bytes());
        reader.read_header().unwrap();
        let mut calls = 0;
        let err = for_each_batch(&mut reader, |_, _| {
            calls += 1;
            Err("boom".into())
        })
        .unwrap_err();
        assert_eq!((err.as_str(), calls), ("boom", 1));
        // Header only: nothing to insert.
        let mut reader = CsvReader::new(&b"A\n"[..]);
        reader.read_header().unwrap();
        assert_eq!(for_each_batch(&mut reader, |_, _| Ok(1)).unwrap(), 0);
    }

    #[test]
    fn exported_safe_csv_has_the_apostrophe_prefix_on_reimport() {
        let result = crate::model::QueryResult {
            columns: vec![crate::model::ColumnInfo {
                name: "A".into(),
                data_type: "VARCHAR2".into(),
            }],
            rows: vec![vec![crate::model::Value::Text("=1+1".into())]],
            ..Default::default()
        };
        let safe = crate::export::to_csv_mode(&result, crate::export::CsvMode::SpreadsheetSafe);
        assert_eq!(parse_csv(&safe).unwrap().rows, [["'=1+1"]]);
        let raw = crate::export::to_csv_mode(&result, crate::export::CsvMode::Raw);
        assert_eq!(parse_csv(&raw).unwrap().rows, [["=1+1"]]);
    }
}
