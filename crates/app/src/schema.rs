//! Schema browser helpers for the Toad-style explorer: a schema dropdown,
//! one tab per object category, and a flat, filterable object list under
//! it. Also turns a `DESCRIBE` into a grid-ready [`QueryResult`].

use ora_core::{ColumnDef, ColumnInfo, IndexDef, ObjectKind, QueryResult, SchemaObject, Value};

/// One tab of the object browser. "Procs" groups procedures, functions and
/// packages, as Toad does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectCategory {
    Tables,
    Views,
    Indexes,
    Sequences,
    Programs,
    Triggers,
    Synonyms,
}

impl ObjectCategory {
    pub const ALL: [ObjectCategory; 7] = [
        ObjectCategory::Tables,
        ObjectCategory::Views,
        ObjectCategory::Indexes,
        ObjectCategory::Sequences,
        ObjectCategory::Programs,
        ObjectCategory::Triggers,
        ObjectCategory::Synonyms,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectCategory::Tables => "Tables",
            ObjectCategory::Views => "Views",
            ObjectCategory::Indexes => "Indexes",
            ObjectCategory::Sequences => "Sequences",
            ObjectCategory::Programs => "Procs",
            ObjectCategory::Triggers => "Triggers",
            ObjectCategory::Synonyms => "Synonyms",
        }
    }

    pub fn contains(self, kind: &ObjectKind) -> bool {
        match self {
            ObjectCategory::Tables => *kind == ObjectKind::Table,
            ObjectCategory::Views => *kind == ObjectKind::View,
            ObjectCategory::Indexes => *kind == ObjectKind::Index,
            ObjectCategory::Sequences => *kind == ObjectKind::Sequence,
            ObjectCategory::Programs => matches!(
                kind,
                ObjectKind::Procedure | ObjectKind::Function | ObjectKind::Package
            ),
            ObjectCategory::Triggers => *kind == ObjectKind::Trigger,
            ObjectCategory::Synonyms => *kind == ObjectKind::Synonym,
        }
    }
}

/// Objects of one category whose name contains `filter` (case-insensitive),
/// sorted by name. An empty filter keeps everything.
pub fn objects_in<'a>(
    objects: &'a [SchemaObject],
    category: ObjectCategory,
    filter: &str,
) -> Vec<&'a SchemaObject> {
    let filter = filter.trim().to_lowercase();
    let mut list: Vec<&SchemaObject> = objects
        .iter()
        .filter(|o| category.contains(&o.kind))
        .filter(|o| filter.is_empty() || o.name.to_lowercase().contains(&filter))
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

pub fn count_in(objects: &[SchemaObject], category: ObjectCategory) -> usize {
    objects
        .iter()
        .filter(|o| category.contains(&o.kind))
        .count()
}

/// Tables and views have data to open and columns to describe.
pub fn has_data(kind: &ObjectKind) -> bool {
    kind.is_describable()
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

/// Render a table's indexes as a grid result for the Index view.
pub fn indexes_to_result(indexes: &[IndexDef]) -> QueryResult {
    let column = |name: &str| ColumnInfo {
        name: name.to_string(),
        data_type: "VARCHAR2".to_string(),
    };
    QueryResult {
        columns: vec![
            column("INDEX_NAME"),
            column("UNIQUE"),
            column("COLUMNS"),
            column("TYPE"),
            column("STATUS"),
        ],
        rows: indexes
            .iter()
            .map(|i| {
                vec![
                    Value::Text(i.name.clone()),
                    Value::Text(if i.unique { "YES" } else { "NO" }.to_string()),
                    Value::Text(i.columns.join(", ")),
                    Value::Text(i.index_type.clone()),
                    Value::Text(i.status.clone()),
                ]
            })
            .collect(),
        truncated: false,
        elapsed: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(owner: &str, name: &str, kind: ObjectKind) -> SchemaObject {
        SchemaObject {
            owner: owner.to_string(),
            name: name.to_string(),
            kind,
        }
    }

    #[test]
    fn categories_cover_kinds_and_group_programs() {
        assert!(ObjectCategory::Programs.contains(&ObjectKind::Package));
        assert!(ObjectCategory::Programs.contains(&ObjectKind::Function));
        assert!(!ObjectCategory::Tables.contains(&ObjectKind::View));
        assert_eq!(ObjectCategory::ALL.len(), 7);
    }

    #[test]
    fn objects_in_filters_by_category_and_name_and_sorts() {
        let objects = vec![
            obj("HR", "EMPLOYEES", ObjectKind::Table),
            obj("HR", "EMP_SEQ", ObjectKind::Sequence),
            obj("HR", "DEPARTMENTS", ObjectKind::Table),
            obj("HR", "GET_SAL", ObjectKind::Function),
        ];
        let tables = objects_in(&objects, ObjectCategory::Tables, "");
        let names: Vec<&str> = tables.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["DEPARTMENTS", "EMPLOYEES"]);
        assert_eq!(
            objects_in(&objects, ObjectCategory::Tables, " emp ").len(),
            1
        );
        assert!(objects_in(&objects, ObjectCategory::Tables, "zzz").is_empty());
        assert_eq!(count_in(&objects, ObjectCategory::Programs), 1);
        assert!(has_data(&ObjectKind::View));
        assert!(!has_data(&ObjectKind::Sequence));
    }

    #[test]
    fn describe_result_matches_toad_columns() {
        let defs = vec![
            ColumnDef {
                name: "ID".into(),
                data_type: "NUMBER".into(),
                nullable: false,
                default: None,
            },
            ColumnDef {
                name: "NAME".into(),
                data_type: "VARCHAR2(20)".into(),
                nullable: true,
                default: Some("'-'".into()),
            },
        ];
        let result = describe_to_result(&defs);
        let names: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["COLUMN_NAME", "DATA_TYPE", "NULLABLE", "DEFAULT"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][2], Value::Text("NO".into()));
        assert_eq!(result.rows[0][3], Value::Null);
        assert_eq!(result.rows[1][2], Value::Text("YES".into()));
        assert_eq!(result.rows[1][3], Value::Text("'-'".into()));
        assert!(!result.truncated);
    }
}
