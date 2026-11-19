//! Table data filters for the data tab's filter bar (and Quick Filter).
//!
//! A [`TableFilter`] is a list of `column op value` conditions joined with
//! AND or OR. It turns into an Oracle `SELECT ... WHERE ...` with
//! positional bind variables (`:1`, `:2`, ...): values never become part of
//! the SQL text, so quotes in a value cannot break or inject into the
//! statement. Column names are quoted identifiers.
//!
//! [`TableFilter::matches`] evaluates the same conditions in memory, with
//! SQL's NULL rules, for the demo backend.

use std::cmp::Ordering;

use crate::import::quote_ident;
use crate::model::{ColumnInfo, Value};

/// Comparison operators offered by the filter bar and Quick Filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    pub const ALL: [FilterOp; 14] = [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Lt,
        FilterOp::Gt,
        FilterOp::Le,
        FilterOp::Ge,
        FilterOp::Contains,
        FilterOp::NotContains,
        FilterOp::StartsWith,
        FilterOp::EndsWith,
        FilterOp::In,
        FilterOp::NotIn,
        FilterOp::IsNull,
        FilterOp::IsNotNull,
    ];

    /// Short label for the operator dropdown.
    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Lt => "<",
            FilterOp::Gt => ">",
            FilterOp::Le => "<=",
            FilterOp::Ge => ">=",
            FilterOp::Contains => "Contains",
            FilterOp::NotContains => "Not contains",
            FilterOp::StartsWith => "Has prefix",
            FilterOp::EndsWith => "Has suffix",
            FilterOp::In => "IN",
            FilterOp::NotIn => "NOT IN",
            FilterOp::IsNull => "IS NULL",
            FilterOp::IsNotNull => "IS NOT NULL",
        }
    }

    /// IS NULL / IS NOT NULL take no value.
    pub fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }
}

/// How the active conditions combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterJoin {
    #[default]
    And,
    Or,
}

impl FilterJoin {
    pub fn keyword(self) -> &'static str {
        match self {
            FilterJoin::And => "AND",
            FilterJoin::Or => "OR",
        }
    }
}

/// One `column op value` condition. For IN / NOT IN the value is a
/// comma-separated list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterCondition {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
}

impl FilterCondition {
    pub fn new(column: impl Into<String>, op: FilterOp, value: impl Into<String>) -> Self {
        FilterCondition {
            column: column.into(),
            op,
            value: value.into(),
        }
    }

    /// A condition without a column, or without a value where one is
    /// needed, is skipped rather than turned into `col = NULL`-style SQL
    /// (in Oracle `''` is NULL, so `= ''` would never match anything).
    pub fn is_complete(&self) -> bool {
        !self.column.trim().is_empty() && (!self.op.needs_value() || !self.list_values().is_empty())
    }

    fn list_values(&self) -> Vec<String> {
        match self.op {
            FilterOp::In | FilterOp::NotIn => self
                .value
                .split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect(),
            _ => {
                let v = self.value.trim();
                if v.is_empty() {
                    Vec::new()
                } else {
                    vec![v.to_string()]
                }
            }
        }
    }

    /// SQL for this condition, appending its bind values to `binds`.
    fn to_sql(&self, binds: &mut Vec<String>) -> String {
        let col = quote_ident(self.column.trim());
        let mut bind = |value: String| {
            binds.push(value);
            format!(":{}", binds.len())
        };
        let values = self.list_values();
        let first = values.first().cloned().unwrap_or_default();
        match self.op {
            FilterOp::Eq => format!("{col} = {}", bind(first)),
            FilterOp::Ne => format!("{col} <> {}", bind(first)),
            FilterOp::Lt => format!("{col} < {}", bind(first)),
            FilterOp::Gt => format!("{col} > {}", bind(first)),
            FilterOp::Le => format!("{col} <= {}", bind(first)),
            FilterOp::Ge => format!("{col} >= {}", bind(first)),
            FilterOp::Contains => format!(
                "{col} LIKE {} ESCAPE '\\'",
                bind(format!("%{}%", escape_like(&first)))
            ),
            FilterOp::NotContains => format!(
                "{col} NOT LIKE {} ESCAPE '\\'",
                bind(format!("%{}%", escape_like(&first)))
            ),
            FilterOp::StartsWith => format!(
                "{col} LIKE {} ESCAPE '\\'",
                bind(format!("{}%", escape_like(&first)))
            ),
            FilterOp::EndsWith => format!(
                "{col} LIKE {} ESCAPE '\\'",
                bind(format!("%{}", escape_like(&first)))
            ),
            FilterOp::In | FilterOp::NotIn => {
                let placeholders: Vec<String> = values.into_iter().map(&mut bind).collect();
                let not = if self.op == FilterOp::NotIn {
                    "NOT "
                } else {
                    ""
                };
                format!("{col} {not}IN ({})", placeholders.join(", "))
            }
            FilterOp::IsNull => format!("{col} IS NULL"),
            FilterOp::IsNotNull => format!("{col} IS NOT NULL"),
        }
    }

    /// Evaluate against one cell. `None` is SQL's UNKNOWN (a comparison
    /// with NULL), which a WHERE clause treats as false.
    fn eval(&self, cell: &Value) -> Option<bool> {
        match self.op {
            FilterOp::IsNull => return Some(cell.is_null()),
            FilterOp::IsNotNull => return Some(!cell.is_null()),
            _ => {}
        }
        if cell.is_null() {
            return None;
        }
        let values = self.list_values();
        let first = values.first().map(String::as_str).unwrap_or("");
        let text = cell.display();
        Some(match self.op {
            FilterOp::Eq => compare(cell, first) == Ordering::Equal,
            FilterOp::Ne => compare(cell, first) != Ordering::Equal,
            FilterOp::Lt => compare(cell, first) == Ordering::Less,
            FilterOp::Gt => compare(cell, first) == Ordering::Greater,
            FilterOp::Le => compare(cell, first) != Ordering::Greater,
            FilterOp::Ge => compare(cell, first) != Ordering::Less,
            FilterOp::Contains => text.contains(first),
            FilterOp::NotContains => !text.contains(first),
            FilterOp::StartsWith => text.starts_with(first),
            FilterOp::EndsWith => text.ends_with(first),
            FilterOp::In => values.iter().any(|v| compare(cell, v) == Ordering::Equal),
            FilterOp::NotIn => values.iter().all(|v| compare(cell, v) != Ordering::Equal),
            FilterOp::IsNull | FilterOp::IsNotNull => unreachable!(),
        })
    }
}

/// Numbers compare numerically (as Oracle converts the bind to NUMBER);
/// everything else compares as text.
fn compare(cell: &Value, value: &str) -> Ordering {
    let as_number = match cell {
        Value::Integer(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    match (as_number, value.trim().parse::<f64>()) {
        (Some(a), Ok(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        _ => cell.display().as_str().cmp(value),
    }
}

/// Escape `\`, `%` and `_` so a LIKE pattern matches them literally
/// (paired with `ESCAPE '\'`).
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The filter applied to one data tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableFilter {
    pub conditions: Vec<FilterCondition>,
    pub join: FilterJoin,
}

impl TableFilter {
    pub fn new(conditions: Vec<FilterCondition>, join: FilterJoin) -> Self {
        TableFilter { conditions, join }
    }

    /// Conditions that take part in the WHERE clause.
    pub fn active(&self) -> impl Iterator<Item = &FilterCondition> {
        self.conditions.iter().filter(|c| c.is_complete())
    }

    pub fn is_empty(&self) -> bool {
        self.active().next().is_none()
    }

    /// `WHERE` body (without the keyword) and its binds, or `None` when no
    /// condition is active.
    pub fn where_clause(&self) -> Option<(String, Vec<String>)> {
        let mut binds = Vec::new();
        let parts: Vec<String> = self.active().map(|c| c.to_sql(&mut binds)).collect();
        if parts.is_empty() {
            return None;
        }
        let glue = format!(" {} ", self.join.keyword());
        Some((parts.join(&glue), binds))
    }

    /// Whether a row passes the filter, with SQL NULL semantics. Unknown
    /// column names never match (the database would raise ORA-00904).
    pub fn matches(&self, columns: &[ColumnInfo], row: &[Value]) -> bool {
        let mut active = self.active().peekable();
        if active.peek().is_none() {
            return true;
        }
        let mut results = active.map(|c| {
            let ix = columns
                .iter()
                .position(|col| col.name.eq_ignore_ascii_case(c.column.trim()));
            ix.and_then(|ix| row.get(ix))
                .and_then(|cell| c.eval(cell))
                .unwrap_or(false)
        });
        match self.join {
            FilterJoin::And => results.all(|r| r),
            FilterJoin::Or => results.any(|r| r),
        }
    }
}

/// A statement plus its positional bind values (`:1` is `binds[0]`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundQuery {
    pub sql: String,
    pub binds: Vec<String>,
}

impl BoundQuery {
    /// The SQL followed by one `-- :n = 'value'` comment line per bind,
    /// for the filter bar's SQL preview and the query log.
    pub fn preview(&self) -> String {
        let mut out = self.sql.clone();
        for (ix, value) in self.binds.iter().enumerate() {
            out.push_str(&format!(
                "\n-- :{} = '{}'",
                ix + 1,
                value.replace('\'', "''")
            ));
        }
        out
    }

    /// The SQL with each `:n` bind replaced by its value as a quoted
    /// literal, so it runs as is when pasted into an editor. Text inside
    /// quotes (string literals, quoted identifiers) is left untouched.
    pub fn inlined(&self) -> String {
        let mut out = String::with_capacity(self.sql.len());
        let mut chars = self.sql.chars().peekable();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            match quote {
                Some(q) => {
                    out.push(c);
                    if c == q {
                        quote = None;
                    }
                }
                None if c == '\'' || c == '"' => {
                    quote = Some(c);
                    out.push(c);
                }
                None if c == ':' && chars.peek().is_some_and(char::is_ascii_digit) => {
                    let mut digits = String::new();
                    while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(d);
                        chars.next();
                    }
                    let value = digits
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|ix| self.binds.get(ix));
                    match value {
                        Some(v) => out.push_str(&format!("'{}'", v.replace('\'', "''"))),
                        None => {
                            out.push(':');
                            out.push_str(&digits);
                        }
                    }
                }
                None => out.push(c),
            }
        }
        out
    }
}

/// Most rows one data-tab fetch may ask for; the grid renders them all, so
/// this bounds memory and paint time.
pub const MAX_PAGE_LIMIT: usize = 5000;

/// Which slice of the (filtered) table a data-tab fetch returns: the
/// Limit / Offset boxes at the bottom of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TablePage {
    pub offset: usize,
    pub limit: usize,
    /// Also select each row's `ROWID` as the first column
    /// ([`ROWID_COLUMN`]), which cell edits use to find their row.
    pub rowid: bool,
}

/// Name of the extra first column carrying the row's `ROWID`.
pub const ROWID_COLUMN: &str = "__ROWID__";

impl Default for TablePage {
    fn default() -> Self {
        TablePage {
            offset: 0,
            limit: crate::DEFAULT_MAX_ROWS,
            rowid: false,
        }
    }
}

impl TablePage {
    /// Clamp to a usable page: at least one row, at most
    /// [`MAX_PAGE_LIMIT`].
    pub fn new(offset: usize, limit: usize) -> Self {
        TablePage {
            offset,
            limit: limit.clamp(1, MAX_PAGE_LIMIT),
            rowid: false,
        }
    }

    /// The page after this one.
    pub fn next(self) -> Self {
        TablePage {
            offset: self.offset.saturating_add(self.limit),
            limit: self.limit,
            rowid: self.rowid,
        }
    }

    /// The page before this one (the first page stays first).
    pub fn prev(self) -> Self {
        TablePage {
            offset: self.offset.saturating_sub(self.limit),
            limit: self.limit,
            rowid: self.rowid,
        }
    }

    /// The same page, selecting (or not) the `ROWID` column.
    pub fn with_rowid(self, rowid: bool) -> Self {
        TablePage { rowid, ..self }
    }
}

/// `SELECT *` from `owner.table` with the filter's WHERE clause and an
/// Oracle 12c+ row window. Offset and limit are `usize`s written as
/// literals, so they cannot carry SQL. No `ORDER BY`: that would force a
/// full scan to open a big table, so the order is whatever Oracle returns
/// (Oracle only guarantees a stable order with an explicit sort).
pub fn select_sql(owner: &str, table: &str, filter: &TableFilter, page: TablePage) -> BoundQuery {
    let mut sql = if page.rowid {
        format!(
            "SELECT t.ROWID AS \"{ROWID_COLUMN}\", t.* FROM {}.{} t",
            quote_ident(owner),
            quote_ident(table)
        )
    } else {
        format!(
            "SELECT * FROM {}.{}",
            quote_ident(owner),
            quote_ident(table)
        )
    };
    let mut binds = Vec::new();
    if let Some((clause, b)) = filter.where_clause() {
        sql.push_str(" WHERE ");
        sql.push_str(&clause);
        binds = b;
    }
    if page.offset == 0 {
        sql.push_str(&format!(" FETCH FIRST {} ROWS ONLY", page.limit));
    } else {
        sql.push_str(&format!(
            " OFFSET {} ROWS FETCH NEXT {} ROWS ONLY",
            page.offset, page.limit
        ));
    }
    BoundQuery { sql, binds }
}

#[cfg(test)]
mod tests {

    #[test]
    fn inlined_replaces_binds_outside_quotes() {
        let q = BoundQuery {
            sql: "SELECT ':1' AS \":2\" FROM t WHERE a LIKE :1 ESCAPE '\\' AND b = :2 AND c = :10"
                .into(),
            binds: vec!["%it's%".into(), "x".into()],
        };
        assert_eq!(
            q.inlined(),
            "SELECT ':1' AS \":2\" FROM t WHERE a LIKE '%it''s%' ESCAPE '\\' AND b = 'x' AND c = :10"
        );
    }
    use super::*;

    fn cond(column: &str, op: FilterOp, value: &str) -> FilterCondition {
        FilterCondition::new(column, op, value)
    }

    fn cols() -> Vec<ColumnInfo> {
        ["ID", "STATUS", "TOTAL"]
            .iter()
            .map(|n| ColumnInfo {
                name: n.to_string(),
                data_type: String::new(),
            })
            .collect()
    }

    #[test]
    fn empty_filter_selects_everything() {
        let q = select_sql(
            "HR",
            "ORDERS",
            &TableFilter::default(),
            TablePage::default(),
        );
        assert_eq!(
            q.sql,
            "SELECT * FROM \"HR\".\"ORDERS\" FETCH FIRST 1000 ROWS ONLY"
        );
        assert!(q.binds.is_empty());
    }

    #[test]
    fn rowid_page_selects_rowid_first() {
        let q = select_sql(
            "HR",
            "EMP",
            &TableFilter::default(),
            TablePage::default().with_rowid(true),
        );
        assert_eq!(
            q.sql,
            "SELECT t.ROWID AS \"__ROWID__\", t.* FROM \"HR\".\"EMP\" t FETCH FIRST 1000 ROWS ONLY"
        );
        assert!(TablePage::default().with_rowid(true).next().rowid);
        assert!(TablePage::new(0, 5).with_rowid(true).prev().rowid);
    }

    #[test]
    fn paging_uses_offset_fetch_and_clamps_limit() {
        let none = TableFilter::default();
        let q = select_sql("HR", "T", &none, TablePage::new(300, 100));
        assert_eq!(
            q.sql,
            "SELECT * FROM \"HR\".\"T\" OFFSET 300 ROWS FETCH NEXT 100 ROWS ONLY"
        );
        assert_eq!(TablePage::new(0, 0).limit, 1);
        assert_eq!(TablePage::new(0, 1_000_000).limit, MAX_PAGE_LIMIT);
        let page = TablePage::new(250, 100);
        assert_eq!(page.next().offset, 350);
        assert_eq!(page.prev().offset, 150);
        assert_eq!(TablePage::new(50, 100).prev().offset, 0);
    }

    #[test]
    fn conditions_use_binds_and_join_keyword() {
        let filter = TableFilter::new(
            vec![
                cond("STATUS", FilterOp::Eq, "shipped"),
                cond("TOTAL", FilterOp::Gt, " 100 "),
            ],
            FilterJoin::And,
        );
        let q = select_sql("HR", "ORDERS", &filter, TablePage::new(0, 50));
        assert_eq!(
            q.sql,
            "SELECT * FROM \"HR\".\"ORDERS\" WHERE \"STATUS\" = :1 AND \"TOTAL\" > :2 FETCH FIRST 50 ROWS ONLY"
        );
        assert_eq!(q.binds, vec!["shipped", "100"]);
        let or = TableFilter::new(filter.conditions.clone(), FilterJoin::Or);
        assert!(or.where_clause().unwrap().0.contains(" OR "));
    }

    #[test]
    fn values_never_enter_the_sql_text() {
        let filter = TableFilter::new(
            vec![cond("STATUS", FilterOp::Eq, "x' OR '1'='1")],
            FilterJoin::And,
        );
        let (sql, binds) = filter.where_clause().unwrap();
        assert_eq!(sql, "\"STATUS\" = :1");
        assert_eq!(binds, vec!["x' OR '1'='1"]);
    }

    #[test]
    fn like_operators_escape_wildcards() {
        let filter = TableFilter::new(
            vec![
                cond("A", FilterOp::Contains, "50%_off"),
                cond("B", FilterOp::StartsWith, "ab"),
                cond("C", FilterOp::EndsWith, "z"),
                cond("D", FilterOp::NotContains, "q"),
            ],
            FilterJoin::And,
        );
        let (sql, binds) = filter.where_clause().unwrap();
        assert_eq!(
            sql,
            "\"A\" LIKE :1 ESCAPE '\\' AND \"B\" LIKE :2 ESCAPE '\\' AND \"C\" LIKE :3 ESCAPE '\\' AND \"D\" NOT LIKE :4 ESCAPE '\\'"
        );
        assert_eq!(binds, vec!["%50\\%\\_off%", "ab%", "%z", "%q%"]);
    }

    #[test]
    fn in_lists_and_null_checks() {
        let filter = TableFilter::new(
            vec![
                cond("STATUS", FilterOp::In, "paid, shipped,,"),
                cond("ID", FilterOp::NotIn, "1"),
                cond("TOTAL", FilterOp::IsNull, ""),
                cond("ID", FilterOp::IsNotNull, "ignored"),
            ],
            FilterJoin::Or,
        );
        let (sql, binds) = filter.where_clause().unwrap();
        assert_eq!(
            sql,
            "\"STATUS\" IN (:1, :2) OR \"ID\" NOT IN (:3) OR \"TOTAL\" IS NULL OR \"ID\" IS NOT NULL"
        );
        assert_eq!(binds, vec!["paid", "shipped", "1"]);
    }

    #[test]
    fn incomplete_conditions_are_skipped() {
        let filter = TableFilter::new(
            vec![
                cond("STATUS", FilterOp::Eq, "  "),
                cond("", FilterOp::IsNull, ""),
                cond("ID", FilterOp::In, " , "),
            ],
            FilterJoin::And,
        );
        assert!(filter.is_empty());
        assert_eq!(filter.where_clause(), None);
    }

    #[test]
    fn preview_lists_binds_with_quotes_doubled() {
        let q = BoundQuery {
            sql: "SELECT 1 FROM dual WHERE x = :1".into(),
            binds: vec!["O'Brien".into()],
        };
        assert_eq!(
            q.preview(),
            "SELECT 1 FROM dual WHERE x = :1\n-- :1 = 'O''Brien'"
        );
    }

    #[test]
    fn matches_follows_sql_semantics() {
        let row = vec![
            Value::Integer(7),
            Value::Text("shipped".into()),
            Value::Null,
        ];
        let check =
            |c: FilterCondition| TableFilter::new(vec![c], FilterJoin::And).matches(&cols(), &row);
        assert!(check(cond("status", FilterOp::Eq, "shipped")));
        assert!(check(cond("ID", FilterOp::Gt, "5")));
        assert!(!check(cond("ID", FilterOp::Gt, "10")));
        // Numeric, not text, comparison: 7 < 10.
        assert!(check(cond("ID", FilterOp::Lt, "10")));
        assert!(check(cond("STATUS", FilterOp::Contains, "hip")));
        assert!(check(cond("STATUS", FilterOp::StartsWith, "ship")));
        assert!(check(cond("STATUS", FilterOp::In, "paid,shipped")));
        assert!(!check(cond("STATUS", FilterOp::NotIn, "paid,shipped")));
        assert!(check(cond("TOTAL", FilterOp::IsNull, "")));
        // NULL compared with anything is unknown, so not a match.
        assert!(!check(cond("TOTAL", FilterOp::Ne, "1")));
        assert!(!check(cond("TOTAL", FilterOp::NotContains, "1")));
        assert!(!check(cond("NOPE", FilterOp::IsNull, "")));

        let and = TableFilter::new(
            vec![
                cond("STATUS", FilterOp::Eq, "shipped"),
                cond("ID", FilterOp::Gt, "100"),
            ],
            FilterJoin::And,
        );
        assert!(!and.matches(&cols(), &row));
        let or = TableFilter::new(and.conditions.clone(), FilterJoin::Or);
        assert!(or.matches(&cols(), &row));
    }
}
