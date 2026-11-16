use crate::model::QueryResult;

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
        };
        assert_eq!(to_csv(&result), "T\r\ncafé ☕\r\n\"a\rb\"\r\n");
    }
}
