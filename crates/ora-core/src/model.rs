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
    /// Do not record this connection's statements in the query history.
    pub skip_history: bool,
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
    /// A text or binary value longer than the fetch's per-cell byte limit
    /// ([`crate::FetchLimits::max_cell_bytes`]): only its start is kept.
    /// Writing it back (edit, Copy As statements) would lose data, so code
    /// that does must refuse or load the full value first.
    Clipped(Box<ClippedValue>),
}

/// The kept start of a clipped cell, and how long the full value is.
#[derive(Debug, Clone, PartialEq)]
pub struct ClippedValue {
    /// `Value::Text` or `Value::Bytes` holding the start of the value.
    pub preview: Value,
    /// Length of the full value in `unit`s.
    pub full_len: u64,
    pub unit: LenUnit,
}

/// What [`ClippedValue::full_len`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenUnit {
    Bytes,
    /// Oracle character LOB units (UCS-2 code units).
    Chars,
}

impl ClippedValue {
    /// The cell after its start was read separately from the cursor: a plain
    /// text when `text` already covers the whole value, else still clipped
    /// with the longer start.
    pub fn with_preview_text(&self, text: String) -> Value {
        if text.encode_utf16().count() as u64 >= self.full_len {
            Value::Text(text)
        } else {
            Value::Clipped(Box::new(ClippedValue {
                preview: Value::Text(text),
                full_len: self.full_len,
                unit: self.unit,
            }))
        }
    }
}

/// The start of one CLOB cell, read for the grid after the page arrived
/// (see `DbClient::fetch_clob_previews`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClobPreview {
    pub rowid: String,
    pub column: String,
    pub text: String,
}

impl ClippedValue {
    pub fn describe_len(&self) -> String {
        let unit = match self.unit {
            LenUnit::Bytes => "bytes",
            LenUnit::Chars => "chars",
        };
        format!("{} {unit}", self.full_len)
    }
}

/// Largest index `<= max` that is a char boundary of `s`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

impl Value {
    /// A CLOB cell the fetch left without any of its text: clipped, empty,
    /// counted in characters. The data tab fills these in lazily.
    pub fn is_unread_clob(&self) -> bool {
        matches!(
            self,
            Value::Clipped(c)
                if c.unit == LenUnit::Chars
                    && matches!(&c.preview, Value::Text(t) if t.is_empty())
        )
    }

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
            Value::Clipped(c) => match &c.preview {
                // Only the length was fetched (LOB locators past the
                // preview budget, every BLOB).
                Value::Bytes(b) if b.is_empty() => format!("(BLOB, {})", c.describe_len()),
                Value::Text(t) if t.is_empty() => format!("(CLOB, {})", c.describe_len()),
                Value::Bytes(b) => {
                    const PREVIEW_LEN: usize = 32;
                    let shown = &b[..b.len().min(PREVIEW_LEN)];
                    format!("{}... ({})", hex_of(shown), c.describe_len())
                }
                preview => format!("{}… ({}, clipped)", preview.display(), c.describe_len()),
            },
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_clipped(&self) -> bool {
        matches!(self, Value::Clipped(_))
    }

    /// Text cell, keeping at most `max_bytes` bytes (cut at a char
    /// boundary). Copies only the kept part.
    pub fn text_limited(text: &str, max_bytes: usize) -> Value {
        if text.len() <= max_bytes {
            return Value::Text(text.to_string());
        }
        let end = floor_char_boundary(text, max_bytes);
        Value::Clipped(Box::new(ClippedValue {
            preview: Value::Text(text[..end].to_string()),
            full_len: text.len() as u64,
            unit: LenUnit::Bytes,
        }))
    }

    /// Like [`Value::text_limited`] for an owned string, freeing the cut
    /// tail instead of copying.
    pub fn owned_text_limited(mut text: String, max_bytes: usize) -> Value {
        if text.len() <= max_bytes {
            return Value::Text(text);
        }
        let full_len = text.len() as u64;
        text.truncate(floor_char_boundary(&text, max_bytes));
        text.shrink_to_fit();
        Value::Clipped(Box::new(ClippedValue {
            preview: Value::Text(text),
            full_len,
            unit: LenUnit::Bytes,
        }))
    }

    /// Binary cell, keeping at most `max_bytes` bytes.
    pub fn bytes_limited(bytes: &[u8], max_bytes: usize) -> Value {
        if bytes.len() <= max_bytes {
            return Value::Bytes(bytes.to_vec());
        }
        Value::Clipped(Box::new(ClippedValue {
            preview: Value::Bytes(bytes[..max_bytes].to_vec()),
            full_len: bytes.len() as u64,
            unit: LenUnit::Bytes,
        }))
    }

    /// Like [`Value::bytes_limited`] for an owned buffer.
    pub fn owned_bytes_limited(mut bytes: Vec<u8>, max_bytes: usize) -> Value {
        if bytes.len() <= max_bytes {
            return Value::Bytes(bytes);
        }
        let full_len = bytes.len() as u64;
        bytes.truncate(max_bytes);
        bytes.shrink_to_fit();
        Value::Clipped(Box::new(ClippedValue {
            preview: Value::Bytes(bytes),
            full_len,
            unit: LenUnit::Bytes,
        }))
    }

    /// Approximate memory held by this cell: the enum itself plus its heap
    /// data. Used for the per-result and all-tabs byte budgets.
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Value>()
            + match self {
                Value::Text(s) => s.capacity(),
                Value::Bytes(b) => b.capacity(),
                Value::Clipped(c) => std::mem::size_of::<ClippedValue>() + c.preview.heap_bytes(),
                _ => 0,
            }
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
    /// Set together with `truncated` when the fetch stopped because the
    /// result reached [`crate::FetchLimits::max_result_bytes`] rather than
    /// the row cap.
    pub byte_limited: bool,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Approximate memory held by the rows (cells and row vectors).
    pub fn heap_bytes(&self) -> usize {
        self.rows
            .iter()
            .map(|row| {
                std::mem::size_of::<Vec<Value>>()
                    + row.iter().map(Value::heap_bytes).sum::<usize>()
                    + (row.capacity() - row.len()) * std::mem::size_of::<Value>()
            })
            .sum()
    }

    /// Cells whose value was clipped at the per-cell limit.
    pub fn clipped_cells(&self) -> usize {
        self.rows
            .iter()
            .flatten()
            .filter(|v| v.is_clipped())
            .count()
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
    /// A table constraint (primary/unique/foreign key, check).
    Constraint,
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
            "CONSTRAINT" => ObjectKind::Constraint,
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
            ObjectKind::Constraint => "Constraints",
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
            ObjectKind::Constraint => "CONSTRAINT",
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
    /// The table a constraint belongs to.
    pub parent: Option<String>,
    /// Short extra text for the explorer row, e.g. `PRIMARY KEY, ENABLED`.
    pub detail: Option<String>,
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

/// Kind of a table constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    ForeignKey,
    Check,
}

/// One constraint of a table, for Data Transfer. Names are informational:
/// copies are created unnamed so they never collide inside a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDef {
    pub name: String,
    pub kind: ConstraintKind,
    /// Constrained columns in key order (empty for a check that the
    /// backend could not tie to columns).
    pub columns: Vec<String>,
    /// Foreign key: referenced table (same schema as the constraint) and
    /// its columns in the same order.
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// Foreign key delete rule: "NO ACTION", "CASCADE" or "SET NULL".
    pub on_delete: String,
    /// Check: the condition text as the database stored it.
    pub condition: String,
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
    #[test]
    fn unread_clobs_and_their_previews() {
        let clipped = |len: u64, preview: &str, unit| ClippedValue {
            preview: Value::Text(preview.to_string()),
            full_len: len,
            unit,
        };
        let unread = Value::Clipped(Box::new(clipped(10, "", LenUnit::Chars)));
        assert!(unread.is_unread_clob());
        assert!(!Value::Clipped(Box::new(clipped(10, "ab", LenUnit::Chars))).is_unread_clob());
        assert!(!Value::Clipped(Box::new(clipped(10, "", LenUnit::Bytes))).is_unread_clob());
        assert!(!Value::Text(String::new()).is_unread_clob());

        let Value::Clipped(c) = &unread else {
            unreachable!()
        };
        let Value::Clipped(partial) = c.with_preview_text("abcd".into()) else {
            panic!("a start shorter than the value stays clipped");
        };
        assert_eq!(partial.preview, Value::Text("abcd".into()));
        assert_eq!(partial.full_len, 10);
        assert_eq!(
            c.with_preview_text("0123456789".into()),
            Value::Text("0123456789".into())
        );
    }

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
