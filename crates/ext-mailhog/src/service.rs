//! The methods of the MailHog extension (see `docs/extension-protocol.md`).

use devtools::mailhog::{MailHogClient, MessagePage, SearchKind};
use devtools::mailhog_wire::{message_to_value, target_from_value};
use ora_core::store::{parse_json, to_json, Json};
use serde_json::{json, Value};

/// A batch of messages is cut at about this many bytes, so one message of
/// the protocol stays far below the line limit.
const CHUNK_BYTES: usize = 512 * 1024;
/// One message larger than this loses its raw source, then its bodies are
/// cut, so a single huge mail cannot break the line limit.
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const BODY_CUT: usize = 512 * 1024;

#[derive(Default)]
pub struct Service {
    client: Option<MailHogClient>,
}

type Emit<'a> = &'a mut dyn FnMut(Json) -> Result<(), String>;

fn to_value(j: &Json) -> Result<Value, String> {
    serde_json::from_str(&to_json(j)).map_err(|e| e.to_string())
}

fn to_wire(v: &Value) -> Result<Json, String> {
    parse_json(&v.to_string()).map_err(|e| e.to_string())
}

fn text(p: &Value, key: &str) -> Result<String, String> {
    p.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing {key}"))
}

fn start(p: &Value) -> usize {
    p.get("start")
        .and_then(Value::as_u64)
        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX))
}

fn cut(s: &mut Option<String>) {
    if let Some(text) = s {
        if text.len() > BODY_CUT {
            let mut end = BODY_CUT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
    }
}

impl Service {
    fn client(&self) -> Result<&MailHogClient, String> {
        self.client
            .as_ref()
            .ok_or_else(|| "not connected".to_string())
    }

    pub fn handle(&mut self, method: &str, params: &Json, emit: Emit) -> Result<Json, String> {
        let p = to_value(params)?;
        let result = match method {
            "connect" => {
                let target = target_from_value(p.get("target").ok_or("missing target")?)?;
                let client = MailHogClient::new(target);
                // Reaching the API and getting its answer is the whole test.
                let page = client.list(0)?;
                self.client = Some(client);
                json!({ "total": page.total })
            }
            "disconnect" => {
                self.client = None;
                json!({})
            }
            "list" => {
                let page = self.client()?.list(start(&p))?;
                stream_page(page, emit)?
            }
            "search" => {
                let kind = SearchKind::from_key(&text(&p, "kind")?)
                    .ok_or("kind must be containing, from or to")?;
                let page = self
                    .client()?
                    .search(kind, &text(&p, "query")?, start(&p))?;
                stream_page(page, emit)?
            }
            "delete" => {
                self.client()?.delete_message(&text(&p, "id")?)?;
                json!({})
            }
            "delete_all" => {
                self.client()?.delete_all()?;
                json!({})
            }
            other => return Err(format!("unknown method {other}")),
        };
        to_wire(&result)
    }
}

/// Send the messages as `chunk`s of `{"messages": [...]}`, then the total
/// as the result.
fn stream_page(page: MessagePage, emit: Emit) -> Result<Value, String> {
    let mut batch: Vec<Value> = Vec::new();
    let mut bytes = 0usize;
    let count = page.messages.len();
    for mut message in page.messages {
        let mut value = message_to_value(&message);
        let mut size = value.to_string().len();
        if size > MAX_MESSAGE_BYTES {
            message.raw = String::new();
            cut(&mut message.text);
            cut(&mut message.html);
            value = message_to_value(&message);
            size = value.to_string().len();
        }
        bytes += size;
        batch.push(value);
        if bytes >= CHUNK_BYTES {
            send(emit, std::mem::take(&mut batch))?;
            bytes = 0;
        }
    }
    if !batch.is_empty() {
        send(emit, batch)?;
    }
    Ok(json!({ "total": page.total, "count": count }))
}

fn send(emit: Emit, messages: Vec<Value>) -> Result<(), String> {
    emit(to_wire(&json!({ "messages": messages }))?)
}
