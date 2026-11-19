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

/// Parse CSV text. Fails on an empty file, an unterminated quote, or a row
/// whose field count differs from the header (the message names the line).
pub fn parse_csv(text: &str) -> Result<CsvData, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let records = split_records(text, ',')?;
    let mut records = records.into_iter();
    let Some((_, headers)) = records.next() else {
        return Err("file CSV kosong".to_string());
    };
    let headers: Vec<String> = headers.into_iter().map(|h| h.trim().to_string()).collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err("baris header CSV kosong".to_string());
    }
    let mut rows = Vec::new();
    for (line, fields) in records {
        // A trailing blank line is not a record.
        if fields.len() == 1 && fields[0].is_empty() {
            continue;
        }
        if fields.len() != headers.len() {
            return Err(format!(
                "baris {line}: {} kolom, header punya {}",
                fields.len(),
                headers.len()
            ));
        }
        rows.push(fields);
    }
    Ok(CsvData { headers, rows })
}

/// Read spreadsheet clipboard rows, preserving quoted tabs and line breaks.
pub fn clipboard_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(split_records(text, '\t')?
        .into_iter()
        .map(|(_, fields)| fields)
        .collect())
}

fn split_records(text: &str, delimiter: char) -> Result<Vec<(usize, Vec<String>)>, String> {
    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut line = 1usize;
    let mut record_line = 1usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                '\n' => {
                    line += 1;
                    field.push(c);
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => in_quotes = true,
            c if c == delimiter => fields.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                fields.push(std::mem::take(&mut field));
                records.push((record_line, std::mem::take(&mut fields)));
                line += 1;
                record_line = line;
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err(format!("baris {record_line}: tanda kutip tidak ditutup"));
    }
    if !field.is_empty() || !fields.is_empty() || text.ends_with('"') {
        fields.push(field);
        records.push((record_line, fields));
    }
    Ok(records)
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
            "kolom CSV tidak ada di tabel: {}",
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
            elapsed: Default::default(),
            fetch_time: None,
        };
        let csv = parse_csv(&crate::export::to_csv(&result)).unwrap();
        assert_eq!(csv.rows, [["x,\"y\""]]);
    }

    #[test]
    fn rejects_ragged_rows_and_open_quotes() {
        assert!(parse_csv("A,B\n1\n").unwrap_err().contains("baris 2"));
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
}
