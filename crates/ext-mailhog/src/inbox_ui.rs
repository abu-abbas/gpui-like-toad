use devtools::mailhog::{MailHogClient, Message, MessagePage, SearchKind};
use devtools::mime;
use ora_core::extension_ui::{
    Event, EventKind, InputValue, Open, Slot, Tick, MAX_STRING_BYTES, VERSION,
};
use serde_json::{json, Value};

#[derive(Default)]
pub(super) struct Inbox {
    revision: u32,
    rows: Vec<Message>,
    total: u64,
    next_start: usize,
    selected: Option<String>,
    search: String,
    kind: Option<SearchKind>,
    active_tab: String,
}

impl Inbox {
    pub fn seed(&mut self, page: MessagePage) {
        *self = Self::default();
        self.replace_page(page);
    }

    pub fn stopped(&mut self) -> Result<Value, String> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Inbox revision exhausted.")?;
        *self = Self {
            revision,
            ..Self::default()
        };
        Ok(json!({
            "version": VERSION,
            "revision": revision,
            "slots": [
                {"slot": "sidebar", "tree": null},
                {"slot": "explorer", "tree": {"type": "column", "children": [
                    {"type": "text", "text": "Inbox", "style": "heading"},
                    {"type": "text", "text": "Server stopped.", "tone": "muted"},
                    {"type": "button", "id": "start-server", "label": "Start server", "variant": "primary"}
                ]}},
                {"slot": "main", "tree": null}
            ]
        }))
    }

    pub fn handle(
        &mut self,
        client: &MailHogClient,
        read_only: bool,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let mut effects = Vec::new();
        match method {
            "ui.open" => {
                serde_json::from_value::<Open>(params).map_err(|e| e.to_string())?;
            }
            "ui.tick" => {
                let tick: Tick = serde_json::from_value(params).map_err(|e| e.to_string())?;
                if tick.revision == self.revision && self.rows.len() <= devtools::mailhog::PAGE {
                    self.refresh(client)?;
                }
            }
            "ui.event" => {
                let event: Event = serde_json::from_value(params).map_err(|e| e.to_string())?;
                if event.revision != self.revision {
                    return Err("The inbox view has changed.".into());
                }
                match (event.slot, event.node.as_str(), event.event) {
                    (
                        Slot::Explorer,
                        "search",
                        EventKind::Submit { value }
                        | EventKind::Change {
                            value: InputValue::Text(value),
                        },
                    ) => {
                        let search = value.as_str().trim().to_owned();
                        let page = Self::fetch(client, self.kind(), &search, 0)?;
                        self.search = search;
                        self.replace_page(page);
                    }
                    (
                        Slot::Explorer,
                        "search-kind",
                        EventKind::Change {
                            value: InputValue::Text(value),
                        },
                    ) => {
                        let kind =
                            SearchKind::from_key(value.as_str()).ok_or("Unknown search kind.")?;
                        let page = Self::fetch(client, kind, &self.search, 0)?;
                        self.kind = Some(kind);
                        self.replace_page(page);
                    }
                    (Slot::Explorer, "messages", EventKind::Select { item }) => {
                        if !self.rows.iter().any(|mail| mail.id == item.as_str()) {
                            return Err("The message is no longer in the inbox.".into());
                        }
                        self.selected = Some(item.as_str().into());
                        self.active_tab.clear();
                    }
                    (Slot::Explorer, "messages", EventKind::More) => {
                        let page = Self::fetch(client, self.kind(), &self.search, self.next_start)?;
                        self.total = page.total;
                        self.next_start = if page.messages.is_empty() {
                            usize::MAX
                        } else {
                            self.next_start.saturating_add(page.messages.len())
                        };
                        for message in page.messages {
                            if !message.id.trim().is_empty()
                                && message.id.len() <= MAX_STRING_BYTES
                                && !self.rows.iter().any(|row| row.id == message.id)
                                && self.rows.len() < 250
                            {
                                self.rows.push(compact_message(message));
                            }
                        }
                    }
                    (Slot::Main, "message-tabs", EventKind::Tab { item }) => {
                        let tab = item.as_str();
                        if !matches!(tab, "text" | "html" | "headers" | "raw") {
                            return Err("Unknown MailHog tab.".into());
                        }
                        self.active_tab = tab.into();
                    }
                    (Slot::Main, "copy-subject", EventKind::Click { .. }) => {
                        let mail = self.selected().ok_or("Select a message first.")?;
                        effects.push(json!({"type": "copy", "text": clipped(&mail.subject)}));
                    }
                    (Slot::Main, "delete-message", EventKind::Click { .. }) => {
                        if read_only {
                            return Err("This MailHog connection is read-only.".into());
                        }
                        let mail = self.selected().ok_or("Select a message first.")?;
                        client.delete_message(&mail.id)?;
                        self.rows
                            .retain(|row| Some(&row.id) != self.selected.as_ref());
                        self.total = self.total.saturating_sub(1);
                        self.selected = None;
                    }
                    (Slot::Explorer, "refresh", EventKind::Click { .. }) => self.refresh(client)?,
                    _ => return Err("Unknown MailHog UI event.".into()),
                }
            }
            _ => return Err("Unknown MailHog UI method.".into()),
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Inbox revision exhausted.")?;
        Ok(json!({
            "version": VERSION,
            "revision": self.revision,
            "slots": [
                {"slot": "sidebar", "tree": {"type": "badge", "text": "Connected", "tone": "success"}},
                {"slot": "explorer", "tree": self.explorer()},
                {"slot": "main", "tree": self.detail(read_only)}
            ],
            "effects": effects,
            "poll_ms": 5000
        }))
    }

    fn fetch(
        client: &MailHogClient,
        kind: SearchKind,
        search: &str,
        start: usize,
    ) -> Result<MessagePage, String> {
        if search.is_empty() {
            client.list(start)
        } else {
            client.search(kind, search, start)
        }
    }

    fn refresh(&mut self, client: &MailHogClient) -> Result<(), String> {
        let page = Self::fetch(client, self.kind(), &self.search, 0)?;
        self.replace_page(page);
        Ok(())
    }

    fn replace_page(&mut self, page: MessagePage) {
        self.total = page.total;
        self.next_start = if page.messages.is_empty() {
            usize::MAX
        } else {
            page.messages.len()
        };
        self.rows = page
            .messages
            .into_iter()
            .take(250)
            .map(compact_message)
            .collect();
        if self.selected().is_none() {
            self.selected = None;
        }
    }

    fn selected(&self) -> Option<&Message> {
        let id = self.selected.as_ref()?;
        self.rows.iter().find(|mail| &mail.id == id)
    }

    fn kind(&self) -> SearchKind {
        self.kind.unwrap_or(SearchKind::Containing)
    }

    fn explorer(&self) -> Value {
        let rows: Vec<_> = self
            .rows
            .iter()
            .take(250)
            .map(|mail| {
                json!({
                    "id": clipped(&mail.id),
                    "title": short(&mail.subject),
                    "subtitle": short(&mail.from),
                    "selected": self.selected.as_ref() == Some(&mail.id)
                })
            })
            .collect();
        json!({"type": "column", "children": [
            {"type": "text", "text": "Inbox", "style": "heading"},
            {"type": "select", "id": "search-kind", "value": self.kind().key(),
             "options": SearchKind::ALL.map(|kind| [kind.key(), kind.label()])},
            {"type": "input", "id": "search", "placeholder": "Search mail", "value": clipped(&self.search)},
            {"type": "button", "id": "refresh", "label": "Refresh", "variant": "ghost"},
            {"type": "list", "id": "messages", "rows": rows,
             "more": self.rows.len() < 250 && (self.next_start as u64) < self.total}
        ]})
    }

    fn detail(&self, read_only: bool) -> Value {
        let Some(mail) = self.selected() else {
            return json!({"type": "empty", "text": "Select a message to read it."});
        };
        let html = mail
            .html
            .as_deref()
            .map(mime::html_to_text)
            .unwrap_or_default();
        let headers: Vec<_> = mail
            .headers
            .iter()
            .take(100)
            .map(|(key, value)| [short(key), short(value)])
            .collect();
        json!({"type": "column", "children": [
            {"type": "text", "text": clipped(&mail.subject), "style": "heading", "selectable": true},
            {"type": "text", "text": clipped(&mail.from), "tone": "muted"},
            {"type": "tabs", "id": "message-tabs",
             "active": if self.active_tab.is_empty() { "text" } else { &self.active_tab },
             "items": [
                {"id": "text", "label": "Text", "child": {"type": "text", "text": clipped(mail.text.as_deref().unwrap_or("")), "selectable": true}},
                {"id": "html", "label": "HTML", "child": {"type": "text", "text": clipped(&html), "selectable": true}},
                {"id": "headers", "label": "Headers", "child": {"type": "kv", "rows": headers}},
                {"id": "raw", "label": "Raw", "child": {"type": "code", "text": clipped(&mail.raw), "copy": true}}
             ]},
            {"type": "row", "children": [
                {"type": "button", "id": "copy-subject", "label": "Copy subject", "variant": "ghost"},
                {"type": "button", "id": "delete-message", "label": "Delete message", "variant": "danger",
                 "disabled": read_only, "writes": true, "confirm": "Delete this message from MailHog?"}
            ]}
        ]})
    }
}

fn compact_message(mut mail: Message) -> Message {
    fn compact(text: &mut String, limit: usize) {
        let end = clip(text, limit).len();
        text.truncate(end);
        text.shrink_to_fit();
    }
    for body in [&mut mail.text, &mut mail.html].into_iter().flatten() {
        compact(body, MAX_STRING_BYTES);
    }
    compact(&mut mail.raw, MAX_STRING_BYTES);
    compact(&mut mail.from, 512);
    compact(&mut mail.subject, 512);
    mail.headers.truncate(100);
    mail.headers.shrink_to_fit();
    for (name, value) in &mut mail.headers {
        compact(name, 512);
        compact(value, 512);
    }
    mail.to = Vec::new();
    mail.cc = Vec::new();
    mail.attachments = Vec::new();
    mail
}

fn clipped(text: &str) -> &str {
    clip(text, MAX_STRING_BYTES)
}

fn short(text: &str) -> &str {
    clip(text, 512)
}

fn clip(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use ora_core::extension_ui::{decode_update, Node, Origin};

    use super::*;

    #[test]
    fn stopped_view_declares_start_and_ends_polling() {
        let mut inbox = Inbox {
            revision: 7,
            ..Inbox::default()
        };
        let json = inbox.stopped().unwrap();
        let update = decode_update(&json.to_string(), Origin::UserEvent).unwrap();
        assert_eq!(update.revision, 8);
        assert_eq!(update.poll_ms, None);
        let explorer = update.slots.iter().find(|slot| slot.slot == Slot::Explorer);
        let Some(Node::Column { children }) = explorer.and_then(|slot| slot.tree.as_ref()) else {
            panic!("expected a stopped view in the explorer");
        };
        assert!(children.iter().any(|node| matches!(node,
            Node::Button { id, disabled: false, .. } if id.as_str() == "start-server"
        )));
    }
}
