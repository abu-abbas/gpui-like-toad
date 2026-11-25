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

use crate::dialect::DbKind;
use crate::filter::{
    FilterCondition, FilterJoin, FilterOp, TableFilter, TablePage, MAX_PAGE_LIMIT,
};
use crate::model::{
    ClippedValue, ColumnDef, ColumnInfo, ConnectionAccess, ConnectionEnvironment,
    ConnectionProfile, ConstraintDef, ConstraintKind, IndexDef, LenUnit, ObjectKind, PlanNode,
    QueryResult, SchemaObject, Value,
};
use crate::store::Json;
use crate::DbInfo;
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
    if !r.order_by.is_empty() {
        pairs.push(("order_by".into(), strings_to_json(&r.order_by)));
    }
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
        order_by: match j.get("order_by") {
            Some(list) => strings_from_json(list)?,
            None => Vec::new(),
        },
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
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err("bad hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "bad hex".to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Database model types
// ---------------------------------------------------------------------------

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn strs(items: &[String]) -> Json {
    Json::Arr(items.iter().cloned().map(Json::Str).collect())
}

fn opt_str(s: &Option<String>) -> Json {
    s.clone().map_or(Json::Null, Json::Str)
}

fn get_str(j: &Json, key: &str) -> Result<String, String> {
    j.get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing or bad text {key}"))
}

fn get_opt_str(j: &Json, key: &str) -> Result<Option<String>, String> {
    match j.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("bad text {key}")),
    }
}

fn get_flag(j: &Json, key: &str) -> bool {
    j.get(key).and_then(Json::as_bool).unwrap_or(false)
}

fn get_strs(j: &Json, key: &str) -> Result<Vec<String>, String> {
    j.get(key)
        .and_then(Json::as_arr)
        .ok_or_else(|| format!("missing list {key}"))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("bad item in {key}"))
        })
        .collect()
}

fn int_of(j: &Json) -> Option<i64> {
    match j {
        Json::Num(n) if n.fract() == 0.0 && n.abs() <= MAX_SAFE as f64 => Some(*n as i64),
        _ => None,
    }
}

fn get_int(j: &Json, key: &str) -> Result<i64, String> {
    j.get(key)
        .and_then(int_of)
        .ok_or_else(|| format!("missing or bad number {key}"))
}

fn get_opt_int(j: &Json, key: &str) -> Result<Option<i64>, String> {
    match j.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(v) => int_of(v)
            .map(Some)
            .ok_or_else(|| format!("bad number {key}")),
    }
}

fn opt_int(n: Option<i64>) -> Json {
    n.map_or(Json::Null, |n| Json::Num(n as f64))
}

fn list<'a>(j: &'a Json, what: &str) -> Result<&'a [Json], String> {
    j.as_arr().ok_or_else(|| format!("{what} is not a list"))
}

pub fn strings_to_json(items: &[String]) -> Json {
    strs(items)
}

pub fn strings_from_json(j: &Json) -> Result<Vec<String>, String> {
    list(j, "the list")?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_string)
                .ok_or_else(|| "bad text in list".to_string())
        })
        .collect()
}

pub fn profile_to_json(p: &ConnectionProfile) -> Json {
    obj(vec![
        ("name", Json::Str(p.name.clone())),
        ("kind", Json::Str(p.kind.key().into())),
        ("username", Json::Str(p.username.clone())),
        ("connect_string", Json::Str(p.connect_string.clone())),
        (
            "access",
            obj(vec![
                (
                    "environment",
                    Json::Str(p.access.environment.label().into()),
                ),
                ("read_only", Json::Bool(p.access.read_only)),
                (
                    "protect_production",
                    Json::Bool(p.access.protect_production),
                ),
                ("sysdba", Json::Bool(p.access.sysdba)),
                ("skip_history", Json::Bool(p.access.skip_history)),
                (
                    "trust_server_certificate",
                    Json::Bool(p.access.trust_server_certificate),
                ),
                ("allow_legacy_tls", Json::Bool(p.access.allow_legacy_tls)),
            ]),
        ),
    ])
}

pub fn profile_from_json(j: &Json) -> Result<ConnectionProfile, String> {
    let access = j.get("access").ok_or("missing access")?;
    Ok(ConnectionProfile {
        name: get_str(j, "name")?,
        kind: DbKind::from_key(&get_str(j, "kind")?),
        username: get_str(j, "username")?,
        connect_string: get_str(j, "connect_string")?,
        access: ConnectionAccess {
            environment: ConnectionEnvironment::parse(&get_str(access, "environment")?),
            read_only: get_flag(access, "read_only"),
            protect_production: get_flag(access, "protect_production"),
            sysdba: get_flag(access, "sysdba"),
            skip_history: get_flag(access, "skip_history"),
            trust_server_certificate: get_flag(access, "trust_server_certificate"),
            allow_legacy_tls: get_flag(access, "allow_legacy_tls"),
        },
    })
}

pub fn db_info_to_json(i: &DbInfo) -> Json {
    obj(vec![
        ("user", Json::Str(i.user.clone())),
        ("database", Json::Str(i.database.clone())),
        ("version", Json::Str(i.version.clone())),
    ])
}

pub fn db_info_from_json(j: &Json) -> Result<DbInfo, String> {
    Ok(DbInfo {
        user: get_str(j, "user")?,
        database: get_str(j, "database")?,
        version: get_str(j, "version")?,
    })
}

pub fn kind_to_json(k: &ObjectKind) -> Json {
    Json::Str(k.oracle_type().to_string())
}

pub fn kind_from_json(j: &Json) -> Result<ObjectKind, String> {
    j.as_str()
        .map(ObjectKind::from_oracle_type)
        .ok_or_else(|| "bad object kind".to_string())
}

pub fn objects_to_json(items: &[SchemaObject]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|o| {
                obj(vec![
                    ("owner", Json::Str(o.owner.clone())),
                    ("name", Json::Str(o.name.clone())),
                    ("kind", kind_to_json(&o.kind)),
                    ("parent", opt_str(&o.parent)),
                    ("detail", opt_str(&o.detail)),
                ])
            })
            .collect(),
    )
}

pub fn objects_from_json(j: &Json) -> Result<Vec<SchemaObject>, String> {
    list(j, "objects")?
        .iter()
        .map(|o| {
            Ok(SchemaObject {
                owner: get_str(o, "owner")?,
                name: get_str(o, "name")?,
                kind: kind_from_json(o.get("kind").ok_or("missing kind")?)?,
                parent: get_opt_str(o, "parent")?,
                detail: get_opt_str(o, "detail")?,
            })
        })
        .collect()
}

pub fn columns_to_json(items: &[ColumnDef]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|c| {
                obj(vec![
                    ("name", Json::Str(c.name.clone())),
                    ("type", Json::Str(c.data_type.clone())),
                    ("nullable", Json::Bool(c.nullable)),
                    ("default", opt_str(&c.default)),
                ])
            })
            .collect(),
    )
}

pub fn columns_from_json(j: &Json) -> Result<Vec<ColumnDef>, String> {
    list(j, "columns")?
        .iter()
        .map(|c| {
            Ok(ColumnDef {
                name: get_str(c, "name")?,
                data_type: get_str(c, "type")?,
                nullable: get_flag(c, "nullable"),
                default: get_opt_str(c, "default")?,
            })
        })
        .collect()
}

pub fn indexes_to_json(items: &[IndexDef]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|i| {
                obj(vec![
                    ("name", Json::Str(i.name.clone())),
                    ("unique", Json::Bool(i.unique)),
                    ("type", Json::Str(i.index_type.clone())),
                    ("columns", strs(&i.columns)),
                    ("status", Json::Str(i.status.clone())),
                ])
            })
            .collect(),
    )
}

pub fn indexes_from_json(j: &Json) -> Result<Vec<IndexDef>, String> {
    list(j, "indexes")?
        .iter()
        .map(|i| {
            Ok(IndexDef {
                name: get_str(i, "name")?,
                unique: get_flag(i, "unique"),
                index_type: get_str(i, "type")?,
                columns: get_strs(i, "columns")?,
                status: get_str(i, "status")?,
            })
        })
        .collect()
}

const CONSTRAINT_KINDS: [(&str, ConstraintKind); 4] = [
    ("pk", ConstraintKind::PrimaryKey),
    ("unique", ConstraintKind::Unique),
    ("fk", ConstraintKind::ForeignKey),
    ("check", ConstraintKind::Check),
];

pub fn constraints_to_json(items: &[ConstraintDef]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|c| {
                let kind = CONSTRAINT_KINDS
                    .iter()
                    .find(|(_, k)| *k == c.kind)
                    .map_or("check", |(name, _)| name);
                obj(vec![
                    ("name", Json::Str(c.name.clone())),
                    ("kind", Json::Str(kind.into())),
                    ("columns", strs(&c.columns)),
                    ("ref_table", Json::Str(c.ref_table.clone())),
                    ("ref_columns", strs(&c.ref_columns)),
                    ("on_delete", Json::Str(c.on_delete.clone())),
                    ("condition", Json::Str(c.condition.clone())),
                ])
            })
            .collect(),
    )
}

pub fn constraints_from_json(j: &Json) -> Result<Vec<ConstraintDef>, String> {
    list(j, "constraints")?
        .iter()
        .map(|c| {
            let name = get_str(c, "kind")?;
            let kind = CONSTRAINT_KINDS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, k)| *k)
                .ok_or_else(|| format!("unknown constraint kind {name}"))?;
            Ok(ConstraintDef {
                name: get_str(c, "name")?,
                kind,
                columns: get_strs(c, "columns")?,
                ref_table: get_str(c, "ref_table")?,
                ref_columns: get_strs(c, "ref_columns")?,
                on_delete: get_str(c, "on_delete")?,
                condition: get_str(c, "condition")?,
            })
        })
        .collect()
}

pub fn plan_nodes_to_json(items: &[PlanNode]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|n| {
                obj(vec![
                    ("id", Json::Num(n.id as f64)),
                    ("parent_id", opt_int(n.parent_id)),
                    ("operation", Json::Str(n.operation.clone())),
                    ("options", Json::Str(n.options.clone())),
                    ("object_name", Json::Str(n.object_name.clone())),
                    ("cost", opt_int(n.cost)),
                    ("cardinality", opt_int(n.cardinality)),
                ])
            })
            .collect(),
    )
}

pub fn plan_nodes_from_json(j: &Json) -> Result<Vec<PlanNode>, String> {
    list(j, "plan")?
        .iter()
        .map(|n| {
            Ok(PlanNode {
                id: get_int(n, "id")?,
                parent_id: get_opt_int(n, "parent_id")?,
                operation: get_str(n, "operation")?,
                options: get_str(n, "options")?,
                object_name: get_str(n, "object_name")?,
                cost: get_opt_int(n, "cost")?,
                cardinality: get_opt_int(n, "cardinality")?,
            })
        })
        .collect()
}

const FILTER_OPS: [(&str, FilterOp); 14] = [
    ("eq", FilterOp::Eq),
    ("ne", FilterOp::Ne),
    ("lt", FilterOp::Lt),
    ("gt", FilterOp::Gt),
    ("le", FilterOp::Le),
    ("ge", FilterOp::Ge),
    ("contains", FilterOp::Contains),
    ("not_contains", FilterOp::NotContains),
    ("starts_with", FilterOp::StartsWith),
    ("ends_with", FilterOp::EndsWith),
    ("in", FilterOp::In),
    ("not_in", FilterOp::NotIn),
    ("is_null", FilterOp::IsNull),
    ("is_not_null", FilterOp::IsNotNull),
];

pub fn filter_to_json(f: &TableFilter) -> Json {
    obj(vec![
        (
            "join",
            Json::Str(
                match f.join {
                    FilterJoin::And => "and",
                    FilterJoin::Or => "or",
                }
                .into(),
            ),
        ),
        (
            "conditions",
            Json::Arr(
                f.conditions
                    .iter()
                    .map(|c| {
                        let op = FILTER_OPS
                            .iter()
                            .find(|(_, o)| *o == c.op)
                            .map_or("eq", |(name, _)| name);
                        obj(vec![
                            ("column", Json::Str(c.column.clone())),
                            ("op", Json::Str(op.into())),
                            ("value", Json::Str(c.value.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub fn filter_from_json(j: &Json) -> Result<TableFilter, String> {
    let join = match get_str(j, "join")?.as_str() {
        "and" => FilterJoin::And,
        "or" => FilterJoin::Or,
        other => return Err(format!("unknown join {other}")),
    };
    let conditions = list(
        j.get("conditions").ok_or("missing conditions")?,
        "conditions",
    )?
    .iter()
    .map(|c| {
        let name = get_str(c, "op")?;
        let op = FILTER_OPS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, o)| *o)
            .ok_or_else(|| format!("unknown filter operator {name}"))?;
        Ok(FilterCondition {
            column: get_str(c, "column")?,
            op,
            value: get_str(c, "value")?,
        })
    })
    .collect::<Result<Vec<_>, String>>()?;
    Ok(TableFilter { conditions, join })
}

pub fn page_to_json(p: &TablePage) -> Json {
    obj(vec![
        ("offset", Json::Num(p.offset as f64)),
        ("limit", Json::Num(p.limit as f64)),
        ("rowid", Json::Bool(p.rowid)),
    ])
}

pub fn page_from_json(j: &Json) -> Result<TablePage, String> {
    let n = |k: &str| {
        j.get(k)
            .and_then(Json::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| format!("bad {k}"))
    };
    Ok(TablePage {
        offset: n("offset")?,
        limit: n("limit")?.clamp(1, MAX_PAGE_LIMIT),
        rowid: get_flag(j, "rowid"),
    })
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
            order_by: vec!["ID".into(), "N".into()],
        };
        let back = result_from_json(&parse_json(&to_json(&result_to_json(&r))).unwrap()).unwrap();
        assert_eq!(back.order_by, ["ID", "N"]);
        assert_eq!(back.columns, r.columns);
        assert_eq!(back.rows, r.rows);
        assert!(back.truncated && back.byte_limited);
        assert_eq!(back.elapsed, r.elapsed);
        assert_eq!(back.fetch_time, r.fetch_time);
    }

    /// A result from an extension that predates `order_by` has none.
    #[test]
    fn an_old_result_has_no_order() {
        let j = parse_json(r#"{"columns":[],"rows":[],"truncated":false}"#).unwrap();
        assert!(result_from_json(&j).unwrap().order_by.is_empty());
        let clean = QueryResult::default();
        assert!(result_to_json(&clean).get("order_by").is_none());
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

    fn through_text(j: &Json) -> Json {
        parse_json(&to_json(j)).unwrap()
    }

    #[test]
    fn model_types_round_trip() {
        let profile = ConnectionProfile {
            name: "Mine".into(),
            kind: DbKind::Sqlite,
            username: "u".into(),
            connect_string: "/tmp/a b.db".into(),
            access: ConnectionAccess {
                environment: ConnectionEnvironment::Production,
                read_only: true,
                protect_production: true,
                sysdba: false,
                skip_history: true,
                trust_server_certificate: true,
                allow_legacy_tls: true,
            },
        };
        let back = profile_from_json(&through_text(&profile_to_json(&profile))).unwrap();
        assert_eq!(back, profile);

        let objects = vec![
            SchemaObject {
                owner: "main".into(),
                name: "t".into(),
                kind: ObjectKind::Table,
                parent: None,
                detail: Some("x".into()),
            },
            SchemaObject {
                owner: "main".into(),
                name: "c".into(),
                kind: ObjectKind::Other("WEIRD".into()),
                parent: Some("t".into()),
                detail: None,
            },
        ];
        assert_eq!(
            objects_from_json(&through_text(&objects_to_json(&objects))).unwrap(),
            objects
        );

        let columns = vec![ColumnDef {
            name: "id".into(),
            data_type: "INTEGER".into(),
            nullable: false,
            default: Some("0".into()),
        }];
        assert_eq!(
            columns_from_json(&through_text(&columns_to_json(&columns))).unwrap(),
            columns
        );

        let indexes = vec![IndexDef {
            name: "i".into(),
            unique: true,
            index_type: "NORMAL".into(),
            columns: vec!["a".into(), "b".into()],
            status: "VALID".into(),
        }];
        assert_eq!(
            indexes_from_json(&through_text(&indexes_to_json(&indexes))).unwrap(),
            indexes
        );

        let constraints = vec![
            ConstraintDef {
                name: "pk".into(),
                kind: ConstraintKind::PrimaryKey,
                columns: vec!["id".into()],
                ref_table: String::new(),
                ref_columns: vec![],
                on_delete: String::new(),
                condition: String::new(),
            },
            ConstraintDef {
                name: "fk".into(),
                kind: ConstraintKind::ForeignKey,
                columns: vec!["p".into()],
                ref_table: "parent".into(),
                ref_columns: vec!["id".into()],
                on_delete: "CASCADE".into(),
                condition: String::new(),
            },
        ];
        assert_eq!(
            constraints_from_json(&through_text(&constraints_to_json(&constraints))).unwrap(),
            constraints
        );

        let nodes = vec![PlanNode {
            id: 3,
            parent_id: Some(0),
            operation: "SCAN".into(),
            options: String::new(),
            object_name: "t".into(),
            cost: None,
            cardinality: Some(10),
        }];
        assert_eq!(
            plan_nodes_from_json(&through_text(&plan_nodes_to_json(&nodes))).unwrap(),
            nodes
        );

        let info = DbInfo {
            user: "u".into(),
            database: "d".into(),
            version: "v".into(),
        };
        assert_eq!(
            db_info_from_json(&through_text(&db_info_to_json(&info))).unwrap(),
            info
        );
    }

    #[test]
    fn filters_and_pages_round_trip_for_every_operator() {
        let conditions = FILTER_OPS
            .iter()
            .map(|(_, op)| FilterCondition::new("c", *op, "1,2"))
            .collect();
        let filter = TableFilter::new(conditions, FilterJoin::Or);
        let back = filter_from_json(&through_text(&filter_to_json(&filter))).unwrap();
        assert_eq!(back, filter);
        let page = TablePage {
            offset: 40,
            limit: 20,
            rowid: true,
        };
        assert_eq!(
            page_from_json(&through_text(&page_to_json(&page))).unwrap(),
            page
        );
    }

    #[test]
    fn bad_model_json_is_an_error_not_a_panic() {
        let bad = parse_json(r#"[{"owner":1}]"#).unwrap();
        assert!(objects_from_json(&bad).is_err());
        assert!(objects_from_json(&Json::Null).is_err());
        assert!(columns_from_json(&parse_json("[{}]").unwrap()).is_err());
        let unknown =
            parse_json(r#"{"join":"and","conditions":[{"column":"a","op":"zz","value":""}]}"#)
                .unwrap();
        assert!(filter_from_json(&unknown).is_err());
        let huge = parse_json(r#"{"offset":0,"limit":999999999,"rowid":false}"#).unwrap();
        assert_eq!(page_from_json(&huge).unwrap().limit, MAX_PAGE_LIMIT);
    }
}
