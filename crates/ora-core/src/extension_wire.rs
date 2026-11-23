//! JSON shapes of the extension protocol for the data types that cross
//! it (see `docs/extension-protocol.md`). Both sides use these functions:
//! the host (`RemoteClient`) decodes what an extension sends and encodes
//! what it asks; an extension built on `ora-core` does the opposite.
//!
//! Cells are compact: `null`, a JSON string (text), a JSON integer
//! (a whole number up to 2^53), or a single-key object for everything that
//! JSON numbers cannot hold exactly:
//! `{"i":"<decimal>"}` big integer, `{"f":<n>}` or `{"f":"NaN"}` float,
//! `{"x":"<hex>"}` bytes, `{"c":{"p":<cell>,"n":<len>,"u":"bytes"|"chars"}}`
//! a clipped value. Decoding never panics on odd input; it returns an error.

use crate::model::{ClippedValue, ColumnInfo, LenUnit, QueryResult, Value};
use crate::store::Json;
use crate::FetchLimits;
use std::time::Duration;

const MAX_SAFE: i128 = 1 << 53;

pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Text(s) => Json::Str(s.clone()),
        Value::Integer(n) if (-MAX_SAFE..=MAX_SAFE).contains(n) => Json::Num(*n as f64),
        Value::Integer(n) => one("i", Json::Str(n.to_string())),
        Value::Float(f) if f.is_finite() => one("f", Json::Num(*f)),
        Value::Float(f) => one("f", Json::Str(f.to_string())),
        Value::Bytes(b) => one("x", Json::Str(to_hex(b))),
        Value::Clipped(c) => one(
            "c",
            Json::Obj(vec![
                ("p".into(), value_to_json(&c.preview)),
                ("n".into(), Json::Num(c.full_len as f64)),
                (
                    "u".into(),
                    Json::Str(
                        match c.unit {
                            LenUnit::Bytes => "bytes",
                            LenUnit::Chars => "chars",
                        }
                        .into(),
                    ),
                ),
            ]),
        ),
    }
}

pub fn value_from_json(j: &Json) -> Result<Value, String> {
    Ok(match j {
        Json::Null => Value::Null,
        Json::Str(s) => Value::Text(s.clone()),
        Json::Num(n) if n.fract() == 0.0 && n.abs() <= MAX_SAFE as f64 => {
            Value::Integer(*n as i128)
        }
        Json::Obj(pairs) if pairs.len() == 1 => {
            let (key, inner) = &pairs[0];
            match (key.as_str(), inner) {
                ("i", Json::Str(s)) => Value::Integer(s.parse().map_err(|_| "bad integer")?),
                ("f", Json::Num(n)) => Value::Float(*n),
                ("f", Json::Str(s)) => Value::Float(s.parse().map_err(|_| "bad float")?),
                ("x", Json::Str(s)) => Value::Bytes(from_hex(s)?),
                ("c", c) => {
                    let preview = value_from_json(c.get("p").ok_or("clipped has no preview")?)?;
                    if !matches!(preview, Value::Text(_) | Value::Bytes(_)) {
                        return Err("a clipped preview must be text or bytes".into());
                    }
                    let full_len = c
                        .get("n")
                        .and_then(Json::as_u64)
                        .ok_or("bad clipped length")?;
                    let unit = match c.get("u").and_then(Json::as_str) {
                        Some("bytes") => LenUnit::Bytes,
                        Some("chars") => LenUnit::Chars,
                        _ => return Err("bad clipped unit".into()),
                    };
                    Value::Clipped(Box::new(ClippedValue {
                        preview,
                        full_len,
                        unit,
                    }))
                }
                _ => return Err(format!("unknown cell type {key}")),
            }
        }
        other => return Err(format!("unsupported cell {other:?}")),
    })
}

pub fn result_to_json(r: &QueryResult) -> Json {
    let mut pairs = vec![
        (
            "columns".into(),
            Json::Arr(
                r.columns
                    .iter()
                    .map(|c| {
                        Json::Arr(vec![
                            Json::Str(c.name.clone()),
                            Json::Str(c.data_type.clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "rows".into(),
            Json::Arr(r.rows.iter().map(|row| row_to_json(row)).collect()),
        ),
        ("truncated".into(), Json::Bool(r.truncated)),
        ("byte_limited".into(), Json::Bool(r.byte_limited)),
        ("elapsed_ms".into(), Json::Num(r.elapsed.as_millis() as f64)),
    ];
    if let Some(t) = r.fetch_time {
        pairs.push(("fetch_ms".into(), Json::Num(t.as_millis() as f64)));
    }
    Json::Obj(pairs)
}

pub fn row_to_json(row: &[Value]) -> Json {
    Json::Arr(row.iter().map(value_to_json).collect())
}

pub fn row_from_json(j: &Json) -> Result<Vec<Value>, String> {
    j.as_arr()
        .ok_or("a row is not an array")?
        .iter()
        .map(value_from_json)
        .collect()
}

pub fn result_from_json(j: &Json) -> Result<QueryResult, String> {
    let columns = j
        .get("columns")
        .and_then(Json::as_arr)
        .ok_or("no columns")?
        .iter()
        .map(|c| {
            let pair = c.as_arr().filter(|p| p.len() == 2).ok_or("bad column")?;
            Ok(ColumnInfo {
                name: pair[0].as_str().ok_or("bad column name")?.to_string(),
                data_type: pair[1].as_str().ok_or("bad column type")?.to_string(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let rows = j
        .get("rows")
        .and_then(Json::as_arr)
        .ok_or("no rows")?
        .iter()
        .map(row_from_json)
        .collect::<Result<Vec<_>, _>>()?;
    let ms = |k: &str| j.get(k).and_then(Json::as_u64).map(Duration::from_millis);
    Ok(QueryResult {
        columns,
        rows,
        truncated: j.get("truncated").and_then(Json::as_bool).unwrap_or(false),
        byte_limited: j
            .get("byte_limited")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        elapsed: ms("elapsed_ms").unwrap_or_default(),
        fetch_time: ms("fetch_ms"),
    })
}

pub fn limits_to_json(l: &FetchLimits) -> Json {
    Json::Obj(vec![
        ("max_rows".into(), Json::Num(l.max_rows as f64)),
        ("max_cell_bytes".into(), Json::Num(l.max_cell_bytes as f64)),
        (
            "max_result_bytes".into(),
            Json::Num(l.max_result_bytes as f64),
        ),
        ("read_lobs".into(), Json::Bool(l.read_lobs)),
    ])
}

pub fn limits_from_json(j: &Json) -> Result<FetchLimits, String> {
    let n = |k: &str| {
        j.get(k)
            .and_then(Json::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| format!("bad {k}"))
    };
    Ok(FetchLimits {
        max_rows: n("max_rows")?,
        max_cell_bytes: n("max_cell_bytes")?,
        max_result_bytes: n("max_result_bytes")?,
        read_lobs: j.get("read_lobs").and_then(Json::as_bool).unwrap_or(false),
    })
}

fn one(key: &str, v: Json) -> Json {
    Json::Obj(vec![(key.into(), v)])
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return Err("bad hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "bad hex".to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{parse_json, to_json};

    fn round(v: Value) -> Value {
        // Through text, as on the wire.
        let text = to_json(&value_to_json(&v));
        value_from_json(&parse_json(&text).unwrap()).unwrap()
    }

    #[test]
    fn cells_round_trip_losslessly() {
        for v in [
            Value::Null,
            Value::Text("héllo \"q\"\n".into()),
            Value::Integer(0),
            Value::Integer(-42),
            Value::Integer(1 << 53),
            Value::Integer((1 << 53) + 1),
            Value::Integer(i128::MAX),
            Value::Integer(i128::MIN),
            Value::Float(1.5),
            Value::Float(-0.25),
            Value::Bytes(vec![0, 1, 254, 255]),
            Value::Bytes(vec![]),
            Value::Clipped(Box::new(ClippedValue {
                preview: Value::Text("start".into()),
                full_len: 99_999,
                unit: LenUnit::Chars,
            })),
        ] {
            assert_eq!(round(v.clone()), v);
        }
        match round(Value::Float(f64::INFINITY)) {
            Value::Float(f) => assert!(f.is_infinite()),
            other => panic!("{other:?}"),
        }
        match round(Value::Float(f64::NAN)) {
            Value::Float(f) => assert!(f.is_nan()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_whole_float_stays_a_float() {
        assert_eq!(round(Value::Float(3.0)), Value::Float(3.0));
        assert_eq!(round(Value::Integer(3)), Value::Integer(3));
    }

    #[test]
    fn query_results_round_trip() {
        let r = QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "ID".into(),
                    data_type: "NUMBER".into(),
                },
                ColumnInfo {
                    name: "N".into(),
                    data_type: "TEXT".into(),
                },
            ],
            rows: vec![
                vec![Value::Integer(1), Value::Text("a".into())],
                vec![Value::Integer(2), Value::Null],
            ],
            truncated: true,
            byte_limited: true,
            elapsed: Duration::from_millis(12),
            fetch_time: Some(Duration::from_millis(3)),
        };
        let back = result_from_json(&parse_json(&to_json(&result_to_json(&r))).unwrap()).unwrap();
        assert_eq!(back.columns, r.columns);
        assert_eq!(back.rows, r.rows);
        assert!(back.truncated && back.byte_limited);
        assert_eq!(back.elapsed, r.elapsed);
        assert_eq!(back.fetch_time, r.fetch_time);
    }

    #[test]
    fn limits_round_trip() {
        let l = FetchLimits::rows(77);
        assert_eq!(limits_from_json(&limits_to_json(&l)).unwrap(), l);
    }

    #[test]
    fn odd_input_is_an_error_not_a_panic() {
        for text in [
            r#"{"i":"abc"}"#,
            r#"{"x":"zz"}"#,
            r#"{"x":"abc"}"#,
            r#"{"q":1}"#,
            r#"{"c":{"p":5,"n":1,"u":"bytes"}}"#,
            r#"{"c":{"p":"s","n":1,"u":"parsecs"}}"#,
            r#"1.5"#,
            r#"true"#,
            r#"[1]"#,
        ] {
            assert!(
                value_from_json(&parse_json(text).unwrap()).is_err(),
                "{text}"
            );
        }
        assert!(result_from_json(&parse_json("{}").unwrap()).is_err());
        assert!(row_from_json(&Json::Null).is_err());
    }
}
