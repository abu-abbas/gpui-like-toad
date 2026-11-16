use std::time::Duration;

/// Connection details for one Oracle database.
///
/// The password is deliberately NOT part of this struct: it lives only in the
/// connection form's memory for the current session and is passed separately
/// to [`crate::DbClient::connect`]. Nothing in this crate persists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionProfile {
    /// Display name shown in the UI, e.g. "Exadata DEV".
    pub name: String,
    /// Database username, e.g. "HR".
    pub username: String,
    /// Oracle Net connect string: Easy Connect (`host:1521/service`), a full
    /// connect descriptor, or a `tnsnames.ora` alias.
    pub connect_string: String,
}

/// One cell value from a query result.
///
/// Values are stored losslessly where cheap (integers keep full precision as
/// `i128`; binary data stays bytes) and rendered for display via
/// [`Value::display`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Text(String),
    Integer(i128),
    Float(f64),
    Bytes(Vec<u8>),
}

impl Value {
    /// Text shown in the grid and used by CSV export.
    ///
    /// NULL renders as an empty string; the grid is expected to style NULL
    /// cells differently via [`Value::is_null`].
    pub fn display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Text(s) => s.clone(),
            Value::Integer(n) => n.to_string(),
            Value::Float(f) => format!("{f}"),
            Value::Bytes(b) => {
                const PREVIEW_LEN: usize = 32;
                if b.len() <= PREVIEW_LEN {
                    hex_of(b)
                } else {
                    format!("{}... ({} bytes)", hex_of(&b[..PREVIEW_LEN]), b.len())
                }
            }
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push_str("0x");
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Metadata for one result column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Column name as returned by the database, e.g. "FIRST_NAME".
    pub name: String,
    /// Human-readable database type, e.g. "VARCHAR2(20)" or "NUMBER".
    pub data_type: String,
}

/// A finished SELECT: columns, rows, and whether the fetch was capped.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    /// True when more rows existed than `max_rows` allowed. The UI must show
    /// this; a silently truncated result looks like a wrong result.
    pub truncated: bool,
    /// Wall time of the fetch, measured by the client implementation.
    pub elapsed: Duration,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, row: usize, col: usize) -> Option<&Value> {
        self.rows.get(row)?.get(col)
    }
}

/// Kind of a schema object, as reported by `ALL_OBJECTS.OBJECT_TYPE`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Index,
    Sequence,
    Procedure,
    Function,
    Package,
    Trigger,
    Synonym,
    Other(String),
}

impl ObjectKind {
    pub fn from_oracle_type(raw: &str) -> Self {
        match raw {
            "TABLE" => ObjectKind::Table,
            "VIEW" => ObjectKind::View,
            "INDEX" => ObjectKind::Index,
            "SEQUENCE" => ObjectKind::Sequence,
            "PROCEDURE" => ObjectKind::Procedure,
            "FUNCTION" => ObjectKind::Function,
            "PACKAGE" => ObjectKind::Package,
            "TRIGGER" => ObjectKind::Trigger,
            "SYNONYM" => ObjectKind::Synonym,
            other => ObjectKind::Other(other.to_string()),
        }
    }

    /// Group heading used by the schema browser tree.
    pub fn group_label(&self) -> &str {
        match self {
            ObjectKind::Table => "Tables",
            ObjectKind::View => "Views",
            ObjectKind::Index => "Indexes",
            ObjectKind::Sequence => "Sequences",
            ObjectKind::Procedure => "Procedures",
            ObjectKind::Function => "Functions",
            ObjectKind::Package => "Packages",
            ObjectKind::Trigger => "Triggers",
            ObjectKind::Synonym => "Synonyms",
            ObjectKind::Other(label) => label,
        }
    }

    /// True for objects the schema browser can describe column-by-column.
    pub fn is_describable(&self) -> bool {
        matches!(self, ObjectKind::Table | ObjectKind::View)
    }
}

/// One object in a schema, e.g. `HR.EMPLOYEES` (table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    pub owner: String,
    pub name: String,
    pub kind: ObjectKind,
}

/// One column of a table or view, from `ALL_TAB_COLUMNS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    /// e.g. "VARCHAR2(20)", "NUMBER(8,2)", "DATE".
    pub data_type: String,
    pub nullable: bool,
    /// Default expression as stored by the database, if any.
    pub default: Option<String>,
}

/// One row of an execution plan, as read from `PLAN_TABLE` after
/// `EXPLAIN PLAN`. The flat list forms a tree through `parent_id`: the
/// root (`SELECT STATEMENT`) has `parent_id == None` (PLAN_TABLE id 0).
/// `cost` and `cardinality` are `None` when the optimizer did not report
/// them for that step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    /// e.g. "TABLE ACCESS", "NESTED LOOPS", "SELECT STATEMENT".
    pub operation: String,
    /// e.g. "FULL", "BY INDEX ROWID", "UNIQUE SCAN". Empty when none.
    pub options: String,
    /// Object the step touches, e.g. "EMPLOYEES". Empty when none.
    pub object_name: String,
    pub cost: Option<i64>,
    pub cardinality: Option<i64>,
}

impl PlanNode {
    /// One-line label for the tree row: operation, options, and object.
    pub fn label(&self) -> String {
        let mut label = self.operation.clone();
        if !self.options.is_empty() {
            label.push(' ');
            label.push_str(&self.options);
        }
        if !self.object_name.is_empty() {
            label.push(' ');
            label.push_str(&self.object_name);
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_display_covers_all_variants() {
        assert_eq!(Value::Null.display(), "");
        assert!(Value::Null.is_null());
        assert_eq!(Value::Text("x".into()).display(), "x");
        assert_eq!(Value::Integer(-42).display(), "-42");
        assert_eq!(Value::Float(2.5).display(), "2.5");
        assert_eq!(Value::Bytes(vec![0xde, 0xad]).display(), "0xDEAD");
    }

    #[test]
    fn bytes_display_truncates_long_values() {
        let shown = Value::Bytes(vec![0xab; 64]).display();
        assert!(shown.starts_with("0xABAB"));
        assert!(shown.ends_with("(64 bytes)"));
    }

    #[test]
    fn object_kind_round_trips_oracle_types() {
        assert_eq!(ObjectKind::from_oracle_type("TABLE"), ObjectKind::Table);
        assert_eq!(ObjectKind::Table.group_label(), "Tables");
        assert!(ObjectKind::Table.is_describable());
        assert!(!ObjectKind::Sequence.is_describable());
        assert_eq!(
            ObjectKind::from_oracle_type("MATERIALIZED VIEW"),
            ObjectKind::Other("MATERIALIZED VIEW".into())
        );
    }
}
