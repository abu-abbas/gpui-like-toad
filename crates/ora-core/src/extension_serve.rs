//! The extension side of the protocol (see `docs/extension-protocol.md`):
//! [`serve`] answers Kilat's requests by calling a [`DbClient`]. An
//! extension program is a `main` that builds its driver and calls `serve`
//! on stdin and stdout.

use crate::client::{DbClient, DbError};
use crate::extension_host::{MAX_LINE, PROTOCOL};
use crate::extension_wire as wire;
use crate::model::QueryResult;
use crate::store::{parse_json, to_json, Json};
use crate::FetchLimits;
use std::io::{self, BufRead, Read, Write};

/// A row batch is cut at about this many bytes, so one message stays far
/// below [`MAX_LINE`].
const CHUNK_BYTES: usize = 512 * 1024;

/// Answer requests from `input` until `shutdown` or the end of the input.
/// An open session is closed on the way out.
pub fn serve<R: BufRead, W: Write>(
    client: &mut dyn DbClient,
    id: &str,
    version: &str,
    mut input: R,
    mut out: W,
) -> io::Result<()> {
    let result = serve_loop(client, id, version, &mut input, &mut out);
    client.disconnect();
    result
}

fn serve_loop<R: BufRead, W: Write>(
    client: &mut dyn DbClient,
    ext_id: &str,
    version: &str,
    input: &mut R,
    out: &mut W,
) -> io::Result<()> {
    loop {
        let mut line = String::new();
        let read = input
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.len() > MAX_LINE {
            return Err(invalid("a request is too large"));
        }
        if line.trim().is_empty() {
            continue;
        }
        let request = parse_json(&line).map_err(|e| invalid(&format!("bad request: {e}")))?;
        let id = request
            .get("id")
            .and_then(Json::as_u64)
            .ok_or_else(|| invalid("a request has no id"))?;
        let method = request
            .get("method")
            .and_then(Json::as_str)
            .ok_or_else(|| invalid("a request has no method"))?
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Json::Null);
        match method.as_str() {
            "shutdown" => return Ok(()),
            "hello" => {
                let reply = Json::Obj(vec![
                    ("protocol".into(), Json::Num(PROTOCOL as f64)),
                    ("id".into(), Json::Str(ext_id.into())),
                    ("version".into(), Json::Str(version.into())),
                ]);
                send(out, id, "result", reply)?;
            }
            _ => {
                let mut write_error = None;
                let outcome = handle(client, &method, &params, &mut |chunk| {
                    send(out, id, "chunk", chunk).map_err(|e| {
                        write_error = Some(e);
                        "the host is gone".to_string()
                    })
                });
                if let Some(e) = write_error {
                    return Err(e);
                }
                match outcome {
                    Ok(result) => send(out, id, "result", result)?,
                    Err(message) => {
                        let error = Json::Obj(vec![
                            ("code".into(), Json::Num(-1.0)),
                            ("message".into(), Json::Str(message)),
                        ]);
                        send(out, id, "error", error)?;
                    }
                }
            }
        }
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn send<W: Write>(out: &mut W, id: u64, key: &str, body: Json) -> io::Result<()> {
    let message = Json::Obj(vec![
        ("id".into(), Json::Num(id as f64)),
        (key.into(), body),
    ]);
    let mut line = to_json(&message);
    if line.len() >= MAX_LINE {
        return Err(invalid("a message is too large"));
    }
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}

type Emit<'a> = &'a mut dyn FnMut(Json) -> Result<(), String>;

fn db(e: DbError) -> String {
    e.to_string()
}

fn text(p: &Json, key: &str) -> Result<String, String> {
    p.get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing {key}"))
}

fn binds(p: &Json) -> Result<Vec<String>, String> {
    match p.get("binds") {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(b) => wire::strings_from_json(b),
    }
}

fn limits(p: &Json) -> Result<FetchLimits, String> {
    wire::limits_from_json(p.get("limits").ok_or("missing limits")?)
}

fn empty() -> Json {
    Json::Obj(Vec::new())
}

fn handle(client: &mut dyn DbClient, method: &str, p: &Json, emit: Emit) -> Result<Json, String> {
    match method {
        "connect" => {
            let profile = wire::profile_from_json(p.get("profile").ok_or("missing profile")?)?;
            let password = text(p, "password")?;
            let info = client.connect(&profile, &password).map_err(db)?;
            Ok(wire::db_info_to_json(&info))
        }
        "disconnect" => {
            client.disconnect();
            Ok(empty())
        }
        "query" => {
            let result = client
                .query_binds(&text(p, "sql")?, &binds(p)?, limits(p)?)
                .map_err(db)?;
            stream_result(result, emit)
        }
        "query_table" => {
            let filter = wire::filter_from_json(p.get("filter").ok_or("missing filter")?)?;
            let page = wire::page_from_json(p.get("page").ok_or("missing page")?)?;
            let result = client
                .query_table(&text(p, "owner")?, &text(p, "table")?, &filter, page)
                .map_err(db)?;
            stream_result(result, emit)
        }
        "execute" => {
            let n = client
                .execute_binds(&text(p, "sql")?, &binds(p)?)
                .map_err(db)?;
            Ok(Json::Obj(vec![("affected".into(), Json::Num(n as f64))]))
        }
        "commit" => client.commit().map(|_| empty()).map_err(db),
        "rollback" => client.rollback().map(|_| empty()).map_err(db),
        "list_objects" => client
            .list_objects(&text(p, "owner")?)
            .map(|v| wire::objects_to_json(&v))
            .map_err(db),
        "list_schemas" => client
            .list_schemas()
            .map(|v| wire::strings_to_json(&v))
            .map_err(db),
        "object_ddl" => {
            let kind = wire::kind_from_json(p.get("kind").ok_or("missing kind")?)?;
            let show_storage = p
                .get("show_storage")
                .and_then(Json::as_bool)
                .unwrap_or(false);
            client
                .object_ddl_with(&text(p, "owner")?, &text(p, "name")?, &kind, show_storage)
                .map(Json::Str)
                .map_err(db)
        }
        "describe_table" => client
            .describe_table(&text(p, "owner")?, &text(p, "name")?)
            .map(|v| wire::columns_to_json(&v))
            .map_err(db),
        "list_indexes" => client
            .list_indexes(&text(p, "owner")?, &text(p, "table")?)
            .map(|v| wire::indexes_to_json(&v))
            .map_err(db),
        "list_constraints" => client
            .list_constraints(&text(p, "owner")?, &text(p, "table")?)
            .map(|v| wire::constraints_to_json(&v))
            .map_err(db),
        "trigger_scripts" => client
            .trigger_scripts(&text(p, "owner")?, &text(p, "table")?)
            .map(|v| wire::strings_to_json(&v))
            .map_err(db),
        "explain_plan" => client
            .explain_plan(&text(p, "sql")?)
            .map(|v| wire::strings_to_json(&v))
            .map_err(db),
        "explain_plan_nodes" => client
            .explain_plan_nodes(&text(p, "sql")?)
            .map(|v| wire::plan_nodes_to_json(&v))
            .map_err(db),
        other => Err(format!("unknown method {other}")),
    }
}

/// Send the rows as `chunk`s of `{"rows": [...]}`, then the rest of the
/// result (columns, flags, timing) with no rows.
fn stream_result(result: QueryResult, emit: Emit) -> Result<Json, String> {
    let mut batch: Vec<Json> = Vec::new();
    let mut bytes = 0usize;
    for row in &result.rows {
        batch.push(wire::row_to_json(row));
        bytes += std::mem::size_of_val(row.as_slice())
            + row.iter().map(crate::Value::heap_bytes).sum::<usize>();
        if bytes >= CHUNK_BYTES {
            emit(Json::Obj(vec![(
                "rows".into(),
                Json::Arr(std::mem::take(&mut batch)),
            )]))?;
            bytes = 0;
        }
    }
    if !batch.is_empty() {
        emit(Json::Obj(vec![("rows".into(), Json::Arr(batch))]))?;
    }
    let mut head = result;
    head.rows = Vec::new();
    Ok(wire::result_to_json(&head))
}
