//! Versioned view descriptions shared by extension programs and the Kilat host.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use crate::extension_host::MAX_LINE;

pub const VERSION: u32 = 1;
pub const MAX_DEPTH: usize = 32;
// A tab adds an items array and a tab object in addition to the node.
pub(crate) const MAX_WIRE_DEPTH: usize = 3 * MAX_DEPTH + 8;
pub const MAX_NODES: usize = 5_000;
pub const MAX_STRING_BYTES: usize = 64 * 1024;
pub const MIN_POLL_MS: u32 = 1_000;

/// Every free-form string on the wire is byte bounded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Text(String);

impl Text {
    pub fn new(value: String) -> Result<Self, &'static str> {
        if value.len() > MAX_STRING_BYTES {
            return Err("extension UI string exceeds 64 KiB");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Sidebar,
    Explorer,
    Main,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    Body,
    Caption,
    Mono,
    Heading,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Normal,
    Muted,
    Primary,
    Danger,
    Success,
    Warning,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
    Success,
}

/// A cell of a [`Node::Diff`] table. `old` set: the value changed from
/// `old` to `text`. `removed`: the value is gone (a deleted row).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffCell {
    pub text: Text,
    pub old: Option<Text>,
    #[serde(default)]
    pub removed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListItem {
    pub id: Text,
    pub title: Text,
    pub subtitle: Option<Text>,
    pub trailing: Option<Text>,
    pub icon: Option<Text>,
    #[serde(default)]
    pub selected: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabItem {
    pub id: Text,
    pub label: Text,
    pub child: Node,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuItem {
    pub id: Text,
    pub label: Text,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub writes: bool,
    pub confirm: Option<Text>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Column {
        children: Vec<Node>,
    },
    Row {
        children: Vec<Node>,
    },
    Scroll {
        child: Box<Node>,
    },
    Spacer,
    Divider,
    Text {
        text: Text,
        style: Option<TextStyle>,
        tone: Option<Tone>,
        #[serde(default)]
        selectable: bool,
    },
    Code {
        text: Text,
        language: Option<Text>,
        #[serde(default)]
        copy: bool,
    },
    Badge {
        text: Text,
        tone: Option<Tone>,
    },
    Icon {
        name: Text,
    },
    Link {
        label: Text,
        url: Text,
    },
    Button {
        id: Text,
        label: Text,
        variant: Option<ButtonVariant>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        writes: bool,
        confirm: Option<Text>,
        /// An interface icon name (see `docs/extension-ui-protocol.md`).
        icon: Option<Text>,
    },
    Input {
        id: Text,
        value: Text,
        placeholder: Option<Text>,
        /// Kilat empties the box after a submit (a console line).
        #[serde(default)]
        clear_on_submit: bool,
    },
    Select {
        id: Text,
        value: Text,
        options: Vec<(Text, Text)>,
    },
    Checkbox {
        id: Text,
        label: Text,
        value: bool,
    },
    Menu {
        id: Text,
        label: Text,
        items: Vec<MenuItem>,
    },
    Tabs {
        id: Text,
        active: Text,
        items: Vec<TabItem>,
    },
    List {
        id: Text,
        rows: Vec<ListItem>,
        #[serde(default)]
        more: bool,
    },
    Table {
        columns: Vec<Text>,
        rows: Vec<Vec<Text>>,
    },
    Kv {
        rows: Vec<(Text, Text)>,
    },
    Empty {
        text: Text,
    },
    /// A tinted strip: icon, title and text on the left, `children`
    /// (badges, progress, buttons) on the right.
    Banner {
        tone: Option<Tone>,
        icon: Option<Text>,
        title: Text,
        text: Option<Text>,
        #[serde(default)]
        children: Vec<Node>,
    },
    /// A bar filled to `value` (0 to 1) with a label and a trailing text.
    Progress {
        value: f32,
        tone: Option<Tone>,
        label: Option<Text>,
        trailing: Option<Text>,
    },
    /// A bordered box with a header line and an optional body.
    Card {
        icon: Option<Text>,
        title: Text,
        badge: Option<Text>,
        badge_tone: Option<Tone>,
        subtitle: Option<Text>,
        child: Option<Box<Node>>,
    },
    /// A table whose cells can show an old value struck out next to the new one.
    Diff {
        columns: Vec<Text>,
        rows: Vec<Vec<DiffCell>>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum Effect {
    Toast {
        text: Text,
    },
    Copy {
        text: Text,
    },
    OpenUrl {
        url: Text,
    },
    Confirm {
        id: Text,
        text: Text,
    },
    /// Offer to save a text file: the host asks where, with `name` as the
    /// suggested file name, and writes the parts one after another. Parts
    /// let a file be larger than one string (the reply stays within the
    /// transport limit). Needs Kilat 0.1.68 or later.
    Save {
        name: Text,
        parts: Vec<Text>,
    },
}

impl Effect {
    /// The joined text of a `save` effect.
    pub fn joined(parts: &[Text]) -> String {
        parts.iter().map(Text::as_str).collect()
    }
}

/// Split `text` into strings that fit the per-string limit, at character
/// boundaries, for a `save` effect.
pub fn split_parts(text: &str) -> Vec<Text> {
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(MAX_STRING_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (head, tail) = rest.split_at(end);
        parts.push(Text(head.to_owned()));
        rest = tail;
    }
    parts
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub slot: Slot,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tick {
    pub revision: u32,
}

/// Send the revision the control was rendered from. Row, menu and tab
/// selections identify the item independently of its display label.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub revision: u32,
    pub slot: Slot,
    pub node: Text,
    pub event: EventKind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum EventKind {
    Click { item: Option<Text> },
    Change { value: InputValue },
    Submit { value: Text },
    Select { item: Text },
    More,
    Tab { item: Text },
    Confirm { id: Text, accepted: bool },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputValue {
    Text(Text),
    Bool(bool),
}

/// Missing slots stay as they were. Explicit null clears a slot.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotUpdate {
    pub slot: Slot,
    #[serde(deserialize_with = "deserialize_tree")]
    pub tree: Option<Node>,
}

fn deserialize_tree<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Node>, D::Error> {
    Option::<Node>::deserialize(deserializer)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewUpdate {
    pub version: u32,
    pub revision: u32,
    #[serde(default)]
    pub slots: Vec<SlotUpdate>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    pub poll_ms: Option<u32>,
}

/// The transport caller supplies this from the request it actually sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Open,
    Tick,
    UserEvent,
}

/// Decode before applying any part of an update. A failed validation is a
/// protocol violation and the host should close the extension process.
pub fn decode_update(json: &str, origin: Origin) -> Result<ViewUpdate, String> {
    if json.len() >= MAX_LINE {
        return Err("extension UI response exceeds the transport limit".into());
    }
    let update: ViewUpdate = serde_json::from_str(json).map_err(|e| e.to_string())?;
    update.validate(origin).map_err(str::to_owned)?;
    Ok(update)
}

impl ViewUpdate {
    pub fn validate(&self, origin: Origin) -> Result<(), &'static str> {
        if self.version != VERSION {
            return Err("unsupported extension UI version; update Kilat or the extension");
        }
        if self.poll_ms.is_some_and(|ms| ms < MIN_POLL_MS) {
            return Err("extension UI polling interval is too short");
        }
        let mut budget = MAX_NODES;
        let mut slots = Vec::new();
        for update in &self.slots {
            if slots.contains(&update.slot) {
                return Err("duplicate extension UI slot");
            }
            slots.push(update.slot);
            if let Some(tree) = &update.tree {
                tree.validate(1, &mut budget, &mut HashSet::new())?;
            }
        }
        spend(&mut budget, self.effects.len())?;
        for effect in &self.effects {
            if origin != Origin::UserEvent && !matches!(effect, Effect::Toast { .. }) {
                return Err("extension UI effect requires a user event");
            }
            match effect {
                Effect::OpenUrl { url } => validate_url(url)?,
                Effect::Confirm { id, .. } => validate_id(id)?,
                Effect::Save { name, parts } => {
                    validate_file_name(name)?;
                    spend(&mut budget, parts.len())?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn validate_url(text: &Text) -> Result<(), &'static str> {
    let url = Url::parse(text.as_str()).map_err(|_| "invalid extension UI URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("extension UI links require an HTTP(S) URL without credentials");
    }
    Ok(())
}

/// A bare file name: no folders, nothing hidden, nothing a file system
/// would read specially. The host still asks the user where to save it.
fn validate_file_name(name: &Text) -> Result<(), &'static str> {
    let name = name.as_str();
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err("extension UI file name must be a plain file name");
    }
    Ok(())
}

fn spend(budget: &mut usize, count: usize) -> Result<(), &'static str> {
    *budget = budget
        .checked_sub(count)
        .ok_or("extension UI exceeds the node budget")?;
    Ok(())
}

fn validate_id(id: &Text) -> Result<(), &'static str> {
    if id.as_str().trim().is_empty() {
        return Err("extension UI ID is empty");
    }
    Ok(())
}

fn insert_id<'a>(ids: &mut HashSet<&'a Text>, id: &'a Text) -> Result<(), &'static str> {
    validate_id(id)?;
    if !ids.insert(id) {
        return Err("duplicate extension UI ID");
    }
    Ok(())
}

impl Node {
    fn validate<'a>(
        &'a self,
        depth: usize,
        budget: &mut usize,
        ids: &mut HashSet<&'a Text>,
    ) -> Result<(), &'static str> {
        if depth > MAX_DEPTH {
            return Err("extension UI exceeds the nesting limit");
        }
        spend(budget, 1)?;
        match self {
            Self::Button { id, .. }
            | Self::Input { id, .. }
            | Self::Select { id, .. }
            | Self::Checkbox { id, .. }
            | Self::Menu { id, .. }
            | Self::Tabs { id, .. }
            | Self::List { id, .. } => insert_id(ids, id)?,
            _ => {}
        }
        let mut item_ids = HashSet::new();
        match self {
            Self::Column { children } | Self::Row { children } => {
                for child in children {
                    child.validate(depth + 1, budget, ids)?;
                }
            }
            Self::Scroll { child } => child.validate(depth + 1, budget, ids)?,
            Self::Card {
                child: Some(child), ..
            } => child.validate(depth + 1, budget, ids)?,
            Self::Banner { children, .. } => {
                for child in children {
                    child.validate(depth + 1, budget, ids)?;
                }
            }
            Self::Progress { value, .. } => {
                if !value.is_finite() || !(0.0..=1.0).contains(value) {
                    return Err("extension UI progress is outside 0 to 1");
                }
            }
            Self::Diff { columns, rows } => {
                spend(budget, columns.len())?;
                spend(budget, rows.len())?;
                for row in rows {
                    if row.len() != columns.len() {
                        return Err("extension UI table row does not match its columns");
                    }
                    spend(budget, row.len())?;
                }
            }
            Self::Link { url, .. } => validate_url(url)?,
            Self::Menu { items, .. } => {
                spend(budget, items.len())?;
                for item in items {
                    insert_id(&mut item_ids, &item.id)?;
                }
            }
            Self::Tabs { active, items, .. } => {
                spend(budget, items.len())?;
                for item in items {
                    insert_id(&mut item_ids, &item.id)?;
                    item.child.validate(depth + 1, budget, ids)?;
                }
                if !item_ids.contains(active) {
                    return Err("active extension UI tab is missing");
                }
            }
            Self::List { rows, .. } => {
                spend(budget, rows.len())?;
                for row in rows {
                    insert_id(&mut item_ids, &row.id)?;
                }
            }
            Self::Select { value, options, .. } => {
                spend(budget, options.len())?;
                for (key, _) in options {
                    insert_id(&mut item_ids, key)?;
                }
                if !item_ids.contains(value) {
                    return Err("selected extension UI option is missing");
                }
            }
            Self::Table { columns, rows } => {
                spend(budget, columns.len())?;
                spend(budget, rows.len())?;
                for row in rows {
                    if row.len() != columns.len() {
                        return Err("extension UI table row does not match its columns");
                    }
                    spend(budget, row.len())?;
                }
            }
            Self::Kv { rows } => spend(budget, rows.len())?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn update(tree: Value) -> Value {
        json!({
            "version": 1,
            "revision": 12,
            "slots": [{"slot": "explorer", "tree": tree}]
        })
    }

    fn check(reply: Value, origin: Origin) -> Result<ViewUpdate, String> {
        decode_update(&reply.to_string(), origin)
    }

    #[test]
    fn save_effects_need_a_user_event_and_a_plain_file_name() {
        let reply = |name: &str| {
            let mut reply = update(json!({"type": "empty", "text": "x"}));
            reply["effects"] =
                json!([{"type": "save", "name": name, "parts": ["UPDATE t;\n", "COMMIT;\n"]}]);
            reply
        };
        let ok = check(reply("undo_emp.sql"), Origin::UserEvent).unwrap();
        match &ok.effects[0] {
            Effect::Save { name, parts } => {
                assert_eq!(name.as_str(), "undo_emp.sql");
                assert_eq!(Effect::joined(parts), "UPDATE t;\nCOMMIT;\n");
            }
            other => panic!("{other:?}"),
        }
        assert!(check(reply("undo_emp.sql"), Origin::Tick).is_err());
        for bad in [
            "", "../x.sql", "a/b.sql", "a\\b.sql", ".hidden", "c:x.sql", "a\nb",
        ] {
            assert!(check(reply(bad), Origin::UserEvent).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn split_parts_keeps_characters_whole_and_round_trips() {
        let text = format!("{}{}", "a".repeat(MAX_STRING_BYTES - 1), "é".repeat(40_000));
        let parts = split_parts(&text);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| p.as_str().len() <= MAX_STRING_BYTES));
        assert_eq!(Effect::joined(&parts), text);
        assert!(split_parts("").is_empty());
    }

    #[test]
    fn banner_progress_card_and_diff_validate() {
        let tree = json!({"type": "column", "children": [
            {"type": "banner", "tone": "warning", "icon": "flask", "title": "Waiting",
             "text": "2 rows", "children": [
                {"type": "progress", "value": 0.5, "tone": "warning", "label": "Auto rollback",
                 "trailing": "2:30"},
                {"type": "button", "id": "commit", "label": "Commit", "variant": "success",
                 "icon": "check", "writes": true}
             ]},
            {"type": "card", "icon": "table", "title": "EMP", "badge": "UPDATE",
             "badge_tone": "warning", "child": {"type": "diff", "columns": ["ID", "SALARY"],
                "rows": [[{"text": "1"}, {"text": "110", "old": "100"}],
                         [{"text": "2"}, {"text": "200", "removed": true}]]}}
        ]});
        check(update(tree), Origin::Open).unwrap();
        let bad = json!({"type": "progress", "value": 1.5});
        assert!(check(update(bad), Origin::Open).is_err());
        let ragged = json!({"type": "diff", "columns": ["A", "B"], "rows": [[{"text": "1"}]]});
        assert!(check(update(ragged), Origin::Open).is_err());
    }

    #[test]
    fn inbox_can_update_selection_and_detail_together() {
        let mut reply = update(json!({
            "type": "list", "id": "messages", "rows": [
                {"id": "mail-1", "title": "Welcome", "selected": true}
            ]
        }));
        reply["slots"].as_array_mut().unwrap().push(json!({
            "slot": "main", "tree": {
                "type": "tabs", "id": "detail", "active": "text", "items": [
                    {"id": "text", "label": "Text", "child": {"type": "text", "text": "Hello"}}
                ]
            }
        }));
        let decoded = check(reply, Origin::UserEvent).unwrap();
        assert_eq!(decoded.revision, 12);
        assert_eq!(decoded.slots.len(), 2);
        assert_eq!(decoded.slots[1].slot, Slot::Main);
    }

    #[test]
    fn rejects_duplicate_ids_and_slot_updates() {
        let button = json!({"type": "button", "id": "refresh", "label": "Refresh"});
        let duplicate = update(json!({"type": "column", "children": [button, button]}));
        assert!(check(duplicate, Origin::Open)
            .unwrap_err()
            .contains("duplicate"));
        let mut duplicate = update(json!({"type": "empty", "text": "No messages"}));
        let slots = duplicate["slots"].as_array_mut().unwrap();
        slots.push(slots[0].clone());
        assert!(check(duplicate, Origin::Open)
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn enforces_depth_and_counts_list_rows_in_the_budget() {
        let mut tree = json!({"type": "spacer"});
        for _ in 0..MAX_DEPTH {
            tree = json!({"type": "scroll", "child": tree});
        }
        assert!(check(update(tree), Origin::Open)
            .unwrap_err()
            .contains("nesting"));
        let rows: Vec<_> = (0..MAX_NODES)
            .map(|i| json!({"id": i.to_string(), "title": "A"}))
            .collect();
        let list = update(json!({"type": "list", "id": "messages", "rows": rows}));
        assert!(check(list, Origin::Open).unwrap_err().contains("budget"));
    }

    #[test]
    fn rejects_unknown_nodes_events_and_oversized_strings() {
        assert!(check(
            update(json!({"type": "html", "text": "<b>Hello</b>"})),
            Origin::Open
        )
        .is_err());
        assert!(serde_json::from_value::<Event>(json!({
            "revision": 1, "slot": "main", "node": "main", "event": {"type": "drag"}
        }))
        .is_err());
        let unicode = "é".repeat(MAX_STRING_BYTES / 2 + 1);
        assert!(check(
            update(json!({"type": "text", "text": unicode})),
            Origin::Open
        )
        .unwrap_err()
        .contains("64 KiB"));
    }

    #[test]
    fn accepts_max_depth_in_transport_and_requires_explicit_slot_clear() {
        let mut tree = json!({"type": "text", "text": "Deep detail"});
        for depth in 1..MAX_DEPTH {
            tree = json!({
                "type": "tabs", "id": format!("tabs-{depth}"), "active": "tab",
                "items": [{"id": "tab", "label": "Details", "child": tree}]
            });
        }
        let reply = update(tree);
        let wire = json!({"id": 2, "result": reply}).to_string();
        let parsed = crate::store::parse_json_with_depth(&wire, MAX_WIRE_DEPTH).unwrap();
        let body = crate::store::to_json(parsed.get("result").unwrap());
        assert!(decode_update(&body, Origin::Open).is_ok());

        let mut reply = update(Value::Null);
        assert!(check(reply.clone(), Origin::Open).is_ok());
        reply["slots"][0].as_object_mut().unwrap().remove("tree");
        assert!(check(reply, Origin::Open).is_err());
    }

    #[test]
    fn checks_effect_origin_links_and_polling() {
        let mut reply = update(json!({"type": "empty", "text": "No messages"}));
        reply["effects"] = json!([{"type": "open_url", "url": "https://example.com/mail"}]);
        assert!(check(reply.clone(), Origin::Tick).is_err());
        assert!(check(reply.clone(), Origin::UserEvent).is_ok());
        reply["effects"][0]["url"] = json!("file:///tmp/mail");
        assert!(check(reply, Origin::UserEvent).is_err());
        let mut reply = update(json!({"type": "spacer"}));
        reply["poll_ms"] = json!(MIN_POLL_MS - 1);
        assert!(check(reply, Origin::Open).is_err());
    }
}
