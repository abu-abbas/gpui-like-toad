//! Schema browser helpers: group [`SchemaObject`]s into tree items and turn
//! a `DESCRIBE` result into a grid-ready [`QueryResult`].
//!
//! Tree item ids encode what was clicked: `group:<label>` for group headers,
//! `obj:<owner>:<name>:<group>` for objects. Oracle identifiers cannot contain
//! `:`, so splitting on it is safe.

use gpui_kit::component::tree::TreeItem;
use ora_core::{ColumnDef, ColumnInfo, QueryResult, SchemaObject, Value};

/// Group display order in the tree; kinds outside this list sort after it.
const GROUP_ORDER: &[&str] = &[
    "Tables",
    "Views",
    "Indexes",
    "Sequences",
    "Procedures",
    "Functions",
    "Packages",
    "Triggers",
    "Synonyms",
];

pub fn build_tree_items(objects: &[SchemaObject]) -> Vec<TreeItem> {
    let mut groups: Vec<(String, Vec<&SchemaObject>)> = Vec::new();
    for obj in objects {
        let label = obj.kind.group_label().to_string();
        match groups.iter_mut().find(|(g, _)| *g == label) {
            Some((_, members)) => members.push(obj),
            None => groups.push((label, vec![obj])),
        }
    }
    groups.sort_by_key(|(label, _)| {
        GROUP_ORDER
            .iter()
            .position(|g| g == label)
            .unwrap_or(GROUP_ORDER.len())
    });
    groups
        .into_iter()
        .map(|(label, members)| {
            let children = members
                .into_iter()
                .map(|obj| {
                    TreeItem::new(
                        format!("obj:{}:{}:{}", obj.owner, obj.name, label),
                        obj.name.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let count = children.len();
            TreeItem::new(format!("group:{label}"), format!("{label} ({count})"))
                .children(children)
                .expanded(label == "Tables")
        })
        .collect()
}

/// Parse a clicked tree item id. Returns `(owner, name, group)` for objects,
/// `None` for group headers and anything unrecognized.
pub fn parse_object_id(id: &str) -> Option<(&str, &str, &str)> {
    let mut parts = id.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("obj"), Some(owner), Some(name), Some(group)) => Some((owner, name, group)),
        _ => None,
    }
}

pub fn is_describable_group(group: &str) -> bool {
    matches!(group, "Tables" | "Views")
}

/// Render a DESCRIBE as a grid result with the same columns Toad shows.
pub fn describe_to_result(defs: &[ColumnDef]) -> QueryResult {
    QueryResult {
        columns: vec![
            ColumnInfo {
                name: "COLUMN_NAME".to_string(),
                data_type: "VARCHAR2".to_string(),
            },
            ColumnInfo {
                name: "DATA_TYPE".to_string(),
                data_type: "VARCHAR2".to_string(),
            },
            ColumnInfo {
                name: "NULLABLE".to_string(),
                data_type: "VARCHAR2".to_string(),
            },
            ColumnInfo {
                name: "DEFAULT".to_string(),
                data_type: "VARCHAR2".to_string(),
            },
        ],
        rows: defs
            .iter()
            .map(|d| {
                vec![
                    Value::Text(d.name.clone()),
                    Value::Text(d.data_type.clone()),
                    Value::Text(if d.nullable { "YES" } else { "NO" }.to_string()),
                    d.default.clone().map_or(Value::Null, Value::Text),
                ]
            })
            .collect(),
        truncated: false,
        elapsed: Default::default(),
    }
}
