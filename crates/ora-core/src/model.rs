use std::time::Duration;

use crate::dialect::DbKind;

/// Connection details for one database.
///
/// The password is deliberately NOT part of this struct: it lives only in the
/// connection form's memory for the current session and is passed separately
/// to [`crate::DbClient::connect`]. Nothing in this crate persists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionProfile {
    /// Display name shown in the UI, e.g. "Exadata DEV".
    pub name: String,
    /// Which database engine this profile connects to.
    pub kind: DbKind,
    /// Database username, e.g. "HR".
    pub username: String,
    /// Oracle Net connect string: Easy Connect (`host:1521/service`), a full
    /// connect descriptor, or a `tnsnames.ora` alias.
    pub connect_string: String,
    pub access: ConnectionAccess,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectionAccess {
    pub environment: ConnectionEnvironment,
    pub read_only: bool,
    pub protect_production: bool,
    pub sysdba: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionEnvironment {
    #[default]
    None,
    Development,
    Staging,
    Production,
}

impl ConnectionEnvironment {
    pub const ALL: [Self; 4] = [
        Self::None,
        Self::Development,
        Self::Staging,
        Self::Production,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Development => "DEV",
            Self::Staging => "STAGING",
            Self::Production => "PROD",
        }
    }

    pub fn parse(label: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|env| env.label() == label)
            .unwrap_or_default()
    }
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
    /// Time spent consuming the open cursor, when measured by the driver.
    pub fetch_time: Option<Duration>,
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
    MaterializedView,
    Index,
    Sequence,
    Procedure,
    Function,
    Package,
    Trigger,
    Synonym,
    Type,
    DbLink,
    Other(String),
}

impl ObjectKind {
    pub fn from_oracle_type(raw: &str) -> Self {
        match raw {
            "TABLE" => ObjectKind::Table,
            "VIEW" => ObjectKind::View,
            "MATERIALIZED VIEW" => ObjectKind::MaterializedView,
            "INDEX" => ObjectKind::Index,
            "SEQUENCE" => ObjectKind::Sequence,
            "PROCEDURE" => ObjectKind::Procedure,
            "FUNCTION" => ObjectKind::Function,
            "PACKAGE" => ObjectKind::Package,
            "TRIGGER" => ObjectKind::Trigger,
            "SYNONYM" => ObjectKind::Synonym,
            "TYPE" => ObjectKind::Type,
            "DATABASE LINK" => ObjectKind::DbLink,
            other => ObjectKind::Other(other.to_string()),
        }
    }

    /// Group heading used by the schema browser tree.
    pub fn group_label(&self) -> &str {
        match self {
            ObjectKind::Table => "Tables",
            ObjectKind::View => "Views",
            ObjectKind::MaterializedView => "Materialized Views",
            ObjectKind::Index => "Indexes",
            ObjectKind::Sequence => "Sequences",
            ObjectKind::Procedure => "Procedures",
            ObjectKind::Function => "Functions",
            ObjectKind::Package => "Packages",
            ObjectKind::Trigger => "Triggers",
            ObjectKind::Synonym => "Synonyms",
            ObjectKind::Type => "Types",
            ObjectKind::DbLink => "DB Links",
            ObjectKind::Other(label) => label,
        }
    }

    /// `OBJECT_TYPE` spelling, as used in DDL and by `DBMS_METADATA`.
    pub fn oracle_type(&self) -> &str {
        match self {
            ObjectKind::Table => "TABLE",
            ObjectKind::View => "VIEW",
            ObjectKind::MaterializedView => "MATERIALIZED VIEW",
            ObjectKind::Index => "INDEX",
            ObjectKind::Sequence => "SEQUENCE",
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::Package => "PACKAGE",
            ObjectKind::Trigger => "TRIGGER",
            ObjectKind::Synonym => "SYNONYM",
            ObjectKind::Type => "TYPE",
            ObjectKind::DbLink => "DATABASE LINK",
            ObjectKind::Other(raw) => raw,
        }
    }

    /// True for objects the schema browser can describe column-by-column.
    pub fn is_describable(&self) -> bool {
        matches!(
            self,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        )
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

/// One index on a table, from `ALL_INDEXES` + `ALL_IND_COLUMNS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDef {
    pub name: String,
    pub unique: bool,
    /// e.g. "NORMAL", "BITMAP", "FUNCTION-BASED NORMAL".
    pub index_type: String,
    /// Indexed columns in key order.
    pub columns: Vec<String>,
    /// e.g. "VALID", "UNUSABLE".
    pub status: String,
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

/// Default Oracle listener port.
pub const DEFAULT_PORT: u16 = 1521;

/// Build an Easy Connect string `host:port/service` from separate fields.
/// The port defaults to 1521 when blank; the service part is omitted when
/// blank (the listener's default service).
pub fn compose_easy_connect(host: &str, port: &str, service: &str) -> String {
    let host = host.trim();
    let port = port.trim();
    let service = service.trim();
    let port = if port.is_empty() {
        DEFAULT_PORT.to_string()
    } else {
        port.to_string()
    };
    if service.is_empty() {
        format!("{host}:{port}")
    } else {
        format!("{host}:{port}/{service}")
    }
}

/// Name for a profile saved without one: `user@connect string` for a
/// database with logins, the file name for a SQLite file.
pub fn default_profile_name(kind: DbKind, username: &str, connect_string: &str) -> String {
    match kind {
        DbKind::Sqlite => std::path::Path::new(connect_string)
            .file_name()
            .map_or_else(
                || connect_string.to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
        DbKind::Oracle => format!("{username}@{connect_string}"),
    }
}

/// Split an Easy Connect string back into `(host, port, service)`, for
/// editing a saved profile. Returns `None` for anything else (connect
/// descriptors, TNS aliases), which the form keeps as a raw TNS string.
pub fn parse_easy_connect(s: &str) -> Option<(String, String, String)> {
    let s = s.trim().trim_start_matches("//");
    if s.is_empty() || s.contains('(') || s.contains(' ') {
        return None;
    }
    let (addr, service) = match s.split_once('/') {
        Some((addr, service)) => (addr, service),
        None => (s, ""),
    };
    let (host, port) = match addr.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, port.to_string())
        }
        // A bare word without port or service is a TNS alias.
        None if !service.is_empty() => (addr, DEFAULT_PORT.to_string()),
        _ => return None,
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port, service.to_string()))
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
    fn easy_connect_compose_and_parse_round_trip() {
        assert_eq!(
            compose_easy_connect(" dbdev ", "", "orclpdb"),
            "dbdev:1521/orclpdb"
        );
        assert_eq!(compose_easy_connect("h", "1522", ""), "h:1522");
        assert_eq!(
            parse_easy_connect("dbdev:1521/orclpdb"),
            Some(("dbdev".into(), "1521".into(), "orclpdb".into()))
        );
        assert_eq!(
            parse_easy_connect("dbdev/orclpdb"),
            Some(("dbdev".into(), "1521".into(), "orclpdb".into()))
        );
        assert_eq!(
            parse_easy_connect("h:1522"),
            Some(("h".into(), "1522".into(), String::new()))
        );
        assert_eq!(parse_easy_connect("ORCL_ALIAS"), None);
        assert_eq!(parse_easy_connect("(DESCRIPTION=(ADDRESS=(HOST=h)))"), None);
        assert_eq!(parse_easy_connect(""), None);
    }

    #[test]
    fn object_kind_round_trips_oracle_types() {
        assert_eq!(ObjectKind::from_oracle_type("TABLE"), ObjectKind::Table);
        assert_eq!(ObjectKind::Table.group_label(), "Tables");
        assert!(ObjectKind::Table.is_describable());
        assert!(!ObjectKind::Sequence.is_describable());
        assert!(ObjectKind::MaterializedView.is_describable());
        assert_eq!(
            ObjectKind::MaterializedView.group_label(),
            "Materialized Views"
        );
        for raw in ["TABLE", "VIEW", "PACKAGE", "SYNONYM", "MATERIALIZED VIEW"] {
            assert_eq!(ObjectKind::from_oracle_type(raw).oracle_type(), raw);
        }
        assert_eq!(
            ObjectKind::from_oracle_type("MATERIALIZED VIEW"),
            ObjectKind::MaterializedView
        );
    }
}
