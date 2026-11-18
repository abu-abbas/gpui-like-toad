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
    use rust_xlsxwriter::{Format, Workbook};

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Results").map_err(|e| e.to_string())?;
    let header_format = Format::new().set_bold();
    for (col, column) in result.columns.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, column.name.as_str(), &header_format)
            .map_err(|e| e.to_string())?;
    }
    for (row_ix, row) in result.rows.iter().enumerate() {
        let row_num = (row_ix + 1) as u32;
        for (col_ix, value) in row.iter().enumerate() {
            let col_num = col_ix as u16;
            match value {
                Value::Null => {}
                Value::Integer(n) => {
                    sheet
                        .write_number(row_num, col_num, *n as f64)
                        .map_err(|e| e.to_string())?;
                }
                Value::Float(f) => {
                    sheet
                        .write_number(row_num, col_num, *f)
                        .map_err(|e| e.to_string())?;
                }
                Value::Text(_) | Value::Bytes(_) => {
                    sheet
                        .write_string(row_num, col_num, value.display())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }
    workbook.save_to_buffer().map_err(|e| e.to_string())
}

/// Render a [`QueryResult`] as RFC 4180 CSV: CRLF line endings, fields
/// quoted when they contain a comma, quote, or line break, embedded quotes
/// doubled. NULL renders as an empty field (same as the grid).
pub fn to_csv(result: &QueryResult) -> String {
    let mut out = String::new();
    let header: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    write_record(&mut out, &header);
    for row in &result.rows {
        let cells: Vec<String> = row.iter().map(|v| v.display()).collect();
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        write_record(&mut out, &refs);
    }
    out
}

fn write_record(out: &mut String, fields: &[&str]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&escape(field));
    }
    out.push_str("\r\n");
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
            elapsed: Default::default(),
            fetch_time: None,
        };
        let bytes = to_xlsx(&result).unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04]);
    }
}
