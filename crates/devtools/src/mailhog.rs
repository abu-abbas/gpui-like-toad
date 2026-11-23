//! MailHog HTTP API client (`/api/v2/messages`, `/api/v2/search`,
//! `/api/v1/messages`). Blocking; the app calls it off the UI thread.

use std::time::Duration;

use serde_json::Value;

use crate::mime::{self, decode_charset, decode_header, header_param, media_type};

/// Messages fetched per page.
pub const PAGE: usize = 50;

#[derive(Clone, Debug, Default)]
pub struct MailHogTarget {
    pub host: String,
    pub port: u16,
    pub https: bool,
    /// Basic auth, when MailHog runs with `-auth-file`.
    pub username: String,
    pub password: Option<String>,
}

impl MailHogTarget {
    pub fn base_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{}://{host}:{}",
            if self.https { "https" } else { "http" },
            self.port
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub size: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    /// The Date header as sent.
    pub date: String,
    /// When MailHog received it (RFC 3339).
    pub created: String,
    pub size: u64,
    pub headers: Vec<(String, String)>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
    pub raw: String,
}

impl Message {
    /// First line of the body for the list.
    pub fn snippet(&self) -> String {
        let body = match (&self.text, &self.html) {
            (Some(t), _) => t.clone(),
            (None, Some(h)) => mime::html_to_text(h),
            _ => String::new(),
        };
        body.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(140)
            .collect()
    }

    /// "12:31" today, else "21 Nov" (from `created`, UTC).
    pub fn time_label(&self, today: &str) -> String {
        let (day, time) = self.created.split_once('T').unwrap_or((&self.created, ""));
        if day == today {
            time.get(..5).unwrap_or(time).to_string()
        } else {
            day.to_string()
        }
    }
}

/// Which messages a search matches (MailHog's `kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Containing,
    From,
    To,
}

impl SearchKind {
    pub const ALL: [Self; 3] = [SearchKind::Containing, SearchKind::From, SearchKind::To];

    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Containing => "All mail",
            SearchKind::From => "From",
            SearchKind::To => "To",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    pub fn key(self) -> &'static str {
        match self {
            SearchKind::Containing => "containing",
            SearchKind::From => "from",
            SearchKind::To => "to",
        }
    }
}

/// A page of messages and the total count on the server.
#[derive(Clone, Debug, Default)]
pub struct MessagePage {
    pub total: u64,
    pub messages: Vec<Message>,
}

pub struct MailHogClient {
    agent: ureq::Agent,
    target: MailHogTarget,
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl MailHogClient {
    pub fn new(target: MailHogTarget) -> Self {
        if target.https {
            crate::ensure_crypto_provider();
        }
        // A dev server on this machine or the LAN: never through the HTTP
        // proxy from the environment.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        MailHogClient { agent, target }
    }

    fn auth(&self) -> Option<String> {
        let user = self.target.username.trim();
        (!user.is_empty()).then(|| {
            let pw = self.target.password.as_deref().unwrap_or("");
            format!(
                "Basic {}",
                mime::base64_encode(format!("{user}:{pw}").as_bytes())
            )
        })
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}{path}", self.target.base_url());
        let mut req = self.agent.get(&url);
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", auth);
        }
        let mut resp = req.call().map_err(|e| describe(&url, &e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        match status {
            200 => serde_json::from_str(&body)
                .map_err(|_| format!("{url} did not answer like MailHog (not JSON).")),
            401 => Err("MailHog asked for a username and password (basic auth).".into()),
            404 => Err(format!("{url} is not a MailHog API (404).")),
            code => Err(format!("MailHog answered HTTP {code}.")),
        }
    }

    fn delete(&self, path: &str) -> Result<(), String> {
        let url = format!("{}{path}", self.target.base_url());
        let mut req = self.agent.delete(&url);
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", auth);
        }
        let resp = req.call().map_err(|e| describe(&url, &e.to_string()))?;
        match resp.status().as_u16() {
            200 | 204 => Ok(()),
            404 => Err("The message is already gone.".into()),
            code => Err(format!("MailHog answered HTTP {code}.")),
        }
    }

    /// Newest first, `start` messages skipped.
    pub fn list(&self, start: usize) -> Result<MessagePage, String> {
        let v = self.get_json(&format!("/api/v2/messages?start={start}&limit={PAGE}"))?;
        Ok(parse_page(&v))
    }

    pub fn search(
        &self,
        kind: SearchKind,
        query: &str,
        start: usize,
    ) -> Result<MessagePage, String> {
        let v = self.get_json(&format!(
            "/api/v2/search?kind={}&query={}&start={start}&limit={PAGE}",
            kind.key(),
            pct(query)
        ))?;
        Ok(parse_page(&v))
    }

    pub fn delete_message(&self, id: &str) -> Result<(), String> {
        self.delete(&format!("/api/v1/messages/{}", pct(id)))
    }

    pub fn delete_all(&self) -> Result<(), String> {
        self.delete("/api/v1/messages")
    }
}

fn describe(url: &str, error: &str) -> String {
    if error.contains("onnection refused") || error.contains("ConnectionFailed") {
        format!(
            "Could not reach MailHog at {url}: connection refused. Is MailHog running? If it only listens on IPv6 or IPv4, try localhost, 127.0.0.1 or ::1."
        )
    } else {
        format!("Could not reach MailHog at {url}: {error}")
    }
}

fn parse_page(v: &Value) -> MessagePage {
    MessagePage {
        total: v.get("total").and_then(Value::as_u64).unwrap_or(0),
        messages: v
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(parse_message).collect())
            .unwrap_or_default(),
    }
}

fn header_list(headers: &Value, name: &str) -> Vec<String> {
    headers
        .as_object()
        .and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
        .and_then(Value::as_array)
        .map(|vals| {
            vals.iter()
                .filter_map(Value::as_str)
                .map(decode_header)
                .collect()
        })
        .unwrap_or_default()
}

fn header(headers: &Value, name: &str) -> String {
    header_list(headers, name)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn split_addresses(values: Vec<String>) -> Vec<String> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Decode one leaf part's body by its Content-Transfer-Encoding.
fn decode_body(headers: &Value, body: &str) -> Vec<u8> {
    match header(headers, "Content-Transfer-Encoding")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "base64" => mime::base64_decode(body),
        "quoted-printable" => mime::qp_decode(body, false),
        _ => body.as_bytes().to_vec(),
    }
}

struct Parts {
    text: Option<String>,
    html: Option<String>,
    attachments: Vec<Attachment>,
}

fn walk(headers: &Value, body: &str, mime_tree: Option<&Value>, out: &mut Parts) {
    if let Some(parts) = mime_tree
        .and_then(|m| m.get("Parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            let h = part.get("Headers").unwrap_or(&Value::Null);
            let b = part.get("Body").and_then(Value::as_str).unwrap_or("");
            walk(h, b, part.get("MIME").filter(|m| !m.is_null()), out);
        }
        return;
    }
    let content_type = header(headers, "Content-Type");
    let media = if content_type.is_empty() {
        "text/plain".to_string()
    } else {
        media_type(&content_type)
    };
    if media.starts_with("multipart/") {
        // MailHog did not split it; nothing we can show part by part.
        return;
    }
    let disposition = header(headers, "Content-Disposition");
    let name =
        header_param(&disposition, "filename").or_else(|| header_param(&content_type, "name"));
    let is_attachment = disposition.to_ascii_lowercase().starts_with("attachment")
        || (name.is_some() && !media.starts_with("text/"));
    let bytes = decode_body(headers, body);
    if is_attachment {
        out.attachments.push(Attachment {
            name: name.unwrap_or_else(|| "attachment".into()),
            content_type: media,
            size: bytes.len(),
        });
        return;
    }
    let charset = header_param(&content_type, "charset").unwrap_or_else(|| "utf-8".into());
    match media.as_str() {
        "text/html" if out.html.is_none() => out.html = Some(decode_charset(&bytes, &charset)),
        "text/plain" if out.text.is_none() => out.text = Some(decode_charset(&bytes, &charset)),
        _ if !media.starts_with("text/") => out.attachments.push(Attachment {
            name: name.unwrap_or_else(|| media.clone()),
            content_type: media,
            size: bytes.len(),
        }),
        _ => {}
    }
}

pub fn parse_message(v: &Value) -> Message {
    let content = v.get("Content").unwrap_or(&Value::Null);
    let headers = content.get("Headers").unwrap_or(&Value::Null);
    let body = content.get("Body").and_then(Value::as_str).unwrap_or("");
    let mut parts = Parts {
        text: None,
        html: None,
        attachments: Vec::new(),
    };
    walk(
        headers,
        body,
        v.get("MIME").filter(|m| !m.is_null()),
        &mut parts,
    );
    let mut header_rows: Vec<(String, String)> = headers
        .as_object()
        .map(|h| {
            h.iter()
                .flat_map(|(k, vals)| {
                    vals.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(move |val| (k.clone(), decode_header(val)))
                })
                .collect()
        })
        .unwrap_or_default();
    header_rows.sort_by(|a, b| a.0.cmp(&b.0));
    let envelope_from = v
        .get("From")
        .map(|f| {
            format!(
                "{}@{}",
                f.get("Mailbox").and_then(Value::as_str).unwrap_or(""),
                f.get("Domain").and_then(Value::as_str).unwrap_or("")
            )
        })
        .unwrap_or_default();
    let from = header(headers, "From");
    Message {
        id: v
            .get("ID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        from: if from.is_empty() { envelope_from } else { from },
        to: split_addresses(header_list(headers, "To")),
        cc: split_addresses(header_list(headers, "Cc")),
        subject: header(headers, "Subject"),
        date: header(headers, "Date"),
        created: v
            .get("Created")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        size: content.get("Size").and_then(Value::as_u64).unwrap_or(0),
        headers: header_rows,
        text: parts.text,
        html: parts.html,
        attachments: parts.attachments,
        raw: v
            .get("Raw")
            .and_then(|r| r.get("Data"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipart_message() {
        let v: Value = serde_json::json!({
            "ID": "abc@mailhog.example",
            "From": {"Mailbox": "no-reply", "Domain": "acme.test"},
            "Content": {
                "Headers": {
                    "Subject": ["=?UTF-8?B?UmVzZXQgeW91ciBwYXNzd29yZA==?="],
                    "From": ["Acme App <no-reply@acme.test>"],
                    "To": ["dewi@example.com, budi@example.com"],
                    "Content-Type": ["multipart/mixed; boundary=b1"]
                },
                "Body": "...",
                "Size": 1234
            },
            "Created": "2026-11-21T05:31:08.1Z",
            "MIME": {"Parts": [
                {"Headers": {"Content-Type": ["multipart/alternative; boundary=b2"]}, "Body": "", "MIME": {"Parts": [
                    {"Headers": {"Content-Type": ["text/plain; charset=utf-8"], "Content-Transfer-Encoding": ["quoted-printable"]}, "Body": "Hi Dewi=2C reset here."},
                    {"Headers": {"Content-Type": ["text/html; charset=utf-8"], "Content-Transfer-Encoding": ["base64"]}, "Body": "PHA+SGkgRGV3aTwvcD4="}
                ]}},
                {"Headers": {"Content-Type": ["application/pdf; name=invoice.pdf"], "Content-Disposition": ["attachment; filename=invoice.pdf"], "Content-Transfer-Encoding": ["base64"]}, "Body": "JVBERi0x"}
            ]},
            "Raw": {"Data": "From: x\r\n\r\nbody"}
        });
        let m = parse_message(&v);
        assert_eq!(m.subject, "Reset your password");
        assert_eq!(m.to, ["dewi@example.com", "budi@example.com"]);
        assert_eq!(m.text.as_deref(), Some("Hi Dewi, reset here."));
        assert_eq!(m.html.as_deref(), Some("<p>Hi Dewi</p>"));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].name, "invoice.pdf");
        assert_eq!(m.attachments[0].size, 6);
        assert_eq!(m.raw, "From: x\r\n\r\nbody");
        assert_eq!(m.time_label("2026-11-21"), "05:31");
        assert_eq!(m.time_label("2026-11-22"), "2026-11-21");
        assert_eq!(m.snippet(), "Hi Dewi, reset here.");
    }

    #[test]
    fn plain_single_part() {
        let v: Value = serde_json::json!({
            "ID": "1",
            "Content": {"Headers": {"Subject": ["Hello"]}, "Body": "Just text", "Size": 9},
            "MIME": null,
            "Created": "2026-11-21T01:00:00Z"
        });
        let m = parse_message(&v);
        assert_eq!(m.text.as_deref(), Some("Just text"));
        assert!(m.attachments.is_empty());
    }

    #[test]
    fn base_urls() {
        let mut t = MailHogTarget {
            host: "localhost".into(),
            port: 8025,
            ..Default::default()
        };
        assert_eq!(t.base_url(), "http://localhost:8025");
        t.host = "::1".into();
        t.https = true;
        assert_eq!(t.base_url(), "https://[::1]:8025");
    }
}
