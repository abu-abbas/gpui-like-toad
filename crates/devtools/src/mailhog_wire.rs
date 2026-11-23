//! JSON form of MailHog messages, for the MailHog extension protocol: the
//! extension program encodes with these functions and Kilat decodes with
//! them, so both sides agree by construction.

use serde_json::{json, Value};

use crate::mailhog::{Attachment, MailHogTarget, Message};

/// A message with every field (see `docs/extension-protocol.md`).
pub fn message_to_value(m: &Message) -> Value {
    json!({
        "id": m.id,
        "from": m.from,
        "to": m.to,
        "cc": m.cc,
        "subject": m.subject,
        "date": m.date,
        "created": m.created,
        "size": m.size,
        "headers": m.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "text": m.text,
        "html": m.html,
        "attachments": m.attachments.iter().map(|a| json!({
            "name": a.name,
            "content_type": a.content_type,
            "size": a.size,
        })).collect::<Vec<_>>(),
        "raw": m.raw,
    })
}

fn string(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn optional(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Decode a message; a missing field is empty, a wrong type is an error.
pub fn message_from_value(v: &Value) -> Result<Message, String> {
    if !v.is_object() {
        return Err("a message must be a JSON object".into());
    }
    let headers = v
        .get("headers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    Some((
                        pair.first()?.as_str()?.to_string(),
                        pair.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let attachments = v
        .get("attachments")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| Attachment {
                    name: string(x, "name"),
                    content_type: string(x, "content_type"),
                    size: x.get("size").and_then(Value::as_u64).unwrap_or(0) as usize,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Message {
        id: string(v, "id"),
        from: string(v, "from"),
        to: strings(v, "to"),
        cc: strings(v, "cc"),
        subject: string(v, "subject"),
        date: string(v, "date"),
        created: string(v, "created"),
        size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
        headers,
        text: optional(v, "text"),
        html: optional(v, "html"),
        attachments,
        raw: string(v, "raw"),
    })
}

pub fn target_to_value(t: &MailHogTarget) -> Value {
    json!({
        "host": t.host,
        "port": t.port,
        "https": t.https,
        "username": t.username,
        "password": t.password,
    })
}

pub fn target_from_value(v: &Value) -> Result<MailHogTarget, String> {
    let host = string(v, "host");
    if host.trim().is_empty() {
        return Err("missing host".into());
    }
    let port = v
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
        .ok_or("missing or invalid port")?;
    Ok(MailHogTarget {
        host,
        port,
        https: v.get("https").and_then(Value::as_bool).unwrap_or(false),
        username: string(v, "username"),
        password: optional(v, "password"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Message {
        Message {
            id: "a1".into(),
            from: "ada@example.com".into(),
            to: vec!["bob@example.com".into()],
            cc: vec!["cy@example.com".into()],
            subject: "Hi \u{1F600}".into(),
            date: "Mon, 23 Nov 2026 10:00:00 +0000".into(),
            created: "2026-11-23T10:00:01Z".into(),
            size: 123,
            headers: vec![("Subject".into(), "Hi".into())],
            text: Some("hello".into()),
            html: None,
            attachments: vec![Attachment {
                name: "a.txt".into(),
                content_type: "text/plain".into(),
                size: 5,
            }],
            raw: "raw".into(),
        }
    }

    #[test]
    fn a_message_survives_the_round_trip() {
        let m = sample();
        let back = message_from_value(&message_to_value(&m)).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn a_non_object_message_is_an_error() {
        assert!(message_from_value(&json!([1, 2])).is_err());
    }

    #[test]
    fn a_target_needs_a_host_and_a_port() {
        let t = MailHogTarget {
            host: "localhost".into(),
            port: 8025,
            https: true,
            username: "u".into(),
            password: Some("p".into()),
        };
        let back = target_from_value(&target_to_value(&t)).unwrap();
        assert_eq!(
            (back.host.as_str(), back.port, back.https),
            ("localhost", 8025, true)
        );
        assert_eq!(back.password.as_deref(), Some("p"));
        assert!(target_from_value(&json!({"host": "", "port": 1})).is_err());
        assert!(target_from_value(&json!({"host": "h", "port": 0})).is_err());
        assert!(target_from_value(&json!({"host": "h", "port": 70000})).is_err());
    }
}
