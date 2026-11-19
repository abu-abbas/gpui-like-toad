use std::time::Instant;

use crate::client::{DbClient, DbError, DbInfo};
use crate::dialect::DbKind;
use crate::fetch::{FetchLimits, RowCollector};
use crate::filter::{TableFilter, TablePage};
use crate::model::{
    ColumnDef, ColumnInfo, ConnectionProfile, IndexDef, ObjectKind, PlanNode, QueryResult,
    SchemaObject, Value,
};

/// In-memory fake Oracle used by Demo mode and by tests.
///
/// It answers a small fixed catalog (HR-style `EMPLOYEES` / `DEPARTMENTS`)
/// so the whole GUI — connect, schema tree, run, grid, explain, export — can
/// be exercised with no database at all. Unknown tables are an error that
/// names the demo catalog, so demo behavior never looks like a real result.
pub struct DemoClient {
    connected_user: Option<String>,
    /// Cell edits since the last commit / rollback, and the committed ones:
    /// the demo has no SQL engine, so edits are overlaid on the canned rows.
    staged: Vec<DemoEdit>,
    committed: Vec<DemoEdit>,
    staged_rows: Vec<DemoRow>,
    committed_rows: Vec<DemoRow>,
    next_row: usize,
}

enum DemoRow {
    Insert {
        table: String,
        index: usize,
        fields: Vec<(String, String)>,
    },
    Delete {
        table: String,
        index: usize,
    },
}

/// One edited demo cell: table, row index in the canned data, column.
#[derive(Clone)]
struct DemoEdit {
    table: String,
    row: usize,
    column: String,
    value: Option<String>,
}

/// The demo's ROWID of canned row `row` of `table`.
fn demo_rowid(table: &str, row: usize) -> String {
    format!("DEMO-{}-{row}", table.to_ascii_uppercase())
}

impl DemoClient {
    pub fn new() -> Self {
        DemoClient {
            connected_user: None,
            staged: Vec::new(),
            committed: Vec::new(),
            staged_rows: Vec::new(),
            committed_rows: Vec::new(),
            next_row: 1000000,
        }
    }

    fn require_connected(&self) -> Result<(), DbError> {
        if self.connected_user.is_some() {
            Ok(())
        } else {
            Err(DbError::new("demo: not connected"))
        }
    }
}

impl Default for DemoClient {
    fn default() -> Self {
        Self::new()
    }
}

fn employees_result(limits: FetchLimits) -> QueryResult {
    let columns = vec![
        ColumnInfo {
            name: "EMPLOYEE_ID".into(),
            data_type: "NUMBER".into(),
        },
        ColumnInfo {
            name: "FIRST_NAME".into(),
            data_type: "VARCHAR2(20)".into(),
        },
        ColumnInfo {
            name: "LAST_NAME".into(),
            data_type: "VARCHAR2(25)".into(),
        },
        ColumnInfo {
            name: "EMAIL".into(),
            data_type: "VARCHAR2(25)".into(),
        },
        ColumnInfo {
            name: "HIRE_DATE".into(),
            data_type: "DATE".into(),
        },
        ColumnInfo {
            name: "SALARY".into(),
            data_type: "NUMBER(8,2)".into(),
        },
        ColumnInfo {
            name: "COMMISSION_PCT".into(),
            data_type: "NUMBER(2,2)".into(),
        },
    ];
    let rows = vec![
        vec![
            Value::Integer(100),
            Value::Text("Steven".into()),
            Value::Text("King".into()),
            Value::Text("SKING".into()),
            Value::Text("17-JUN-2003".into()),
            Value::Integer(24000),
            Value::Null,
        ],
        vec![
            Value::Integer(101),
            Value::Text("Neena".into()),
            Value::Text("Kochhar".into()),
            Value::Text("NKOCHHAR".into()),
            Value::Text("21-SEP-2005".into()),
            Value::Integer(17000),
            Value::Null,
        ],
        vec![
            Value::Integer(102),
            Value::Text("Lex".into()),
            Value::Text("De Haan".into()),
            Value::Text("LDEHAAN".into()),
            Value::Text("13-JAN-2001".into()),
            Value::Integer(17000),
            Value::Null,
        ],
        vec![
            Value::Integer(145),
            Value::Text("John".into()),
            Value::Text("Russell".into()),
            Value::Text("JRUSSEL".into()),
            Value::Text("01-OCT-2004".into()),
            Value::Integer(14000),
            Value::Float(0.4),
        ],
        vec![
            Value::Integer(146),
            Value::Text("Karen".into()),
            Value::Text("Partners".into()),
            Value::Text("KPARTNER".into()),
            Value::Text("05-JAN-2005".into()),
            Value::Integer(13500),
            Value::Float(0.3),
        ],
        vec![
            Value::Integer(147),
            Value::Text("Alberto".into()),
            Value::Text("Errazuriz".into()),
            Value::Text("AERRAZUR".into()),
            Value::Text("10-MAR-2005".into()),
            Value::Integer(12000),
            Value::Float(0.3),
        ],
    ];
    finish(columns, rows, limits)
}

fn departments_result(limits: FetchLimits) -> QueryResult {
    let columns = vec![
        ColumnInfo {
            name: "DEPARTMENT_ID".into(),
            data_type: "NUMBER(4)".into(),
        },
        ColumnInfo {
            name: "DEPARTMENT_NAME".into(),
            data_type: "VARCHAR2(30)".into(),
        },
        ColumnInfo {
            name: "LOCATION_ID".into(),
            data_type: "NUMBER(4)".into(),
        },
    ];
    let rows = vec![
        vec![
            Value::Integer(10),
            Value::Text("Administration".into()),
            Value::Integer(1700),
        ],
        vec![
            Value::Integer(20),
            Value::Text("Marketing".into()),
            Value::Integer(1800),
        ],
        vec![
            Value::Integer(30),
            Value::Text("Purchasing".into()),
            Value::Integer(1700),
        ],
        vec![
            Value::Integer(40),
            Value::Text("Human Resources".into()),
            Value::Integer(2400),
        ],
    ];
    finish(columns, rows, limits)
}

fn dual_result(limits: FetchLimits) -> QueryResult {
    let columns = vec![ColumnInfo {
        name: "USER".into(),
        data_type: "VARCHAR2(128)".into(),
    }];
    let rows = vec![vec![Value::Text("DEMO".into())]];
    finish(columns, rows, limits)
}

fn finish(columns: Vec<ColumnInfo>, rows: Vec<Vec<Value>>, limits: FetchLimits) -> QueryResult {
    RowCollector::collect_all(limits, columns, rows, Default::default())
}

impl DbClient for DemoClient {
    /// The demo database answers Oracle-flavored SQL.
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn connect(&mut self, profile: &ConnectionProfile, _password: &str) -> Result<DbInfo, DbError> {
        if profile.username.trim().is_empty() {
            return Err(DbError::new("demo: username is required"));
        }
        let user = profile.username.trim().to_ascii_uppercase();
        self.connected_user = Some(user.clone());
        Ok(DbInfo {
            user,
            database: "DEMO".into(),
            version: "Demo database (no Oracle connection)".into(),
        })
    }

    fn disconnect(&mut self) {
        self.connected_user = None;
    }

    fn is_connected(&self) -> bool {
        self.connected_user.is_some()
    }

    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<QueryResult, DbError> {
        self.require_connected()?;
        let started = Instant::now();
        // Ignore identifier quotes and the owner prefix so the explorer's
        // `SELECT * FROM "HR"."EMPLOYEES"` hits the same demo table as a
        // hand-typed `select * from employees`.
        let lowered = sql.to_ascii_lowercase().replace('"', "");
        let from_clause = lowered.split_once("from ").map_or("", |(_, rest)| rest);
        let table = from_clause
            .split_whitespace()
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("");
        let mut result = if table == "employees" || table == "emp_details_view" {
            employees_result(limits)
        } else if table == "departments" {
            departments_result(limits)
        } else if lowered.contains("dual") {
            dual_result(limits)
        } else {
            return Err(DbError::new(
                "demo: unknown table in query (demo catalog: EMPLOYEES, DEPARTMENTS, DUAL)",
            ));
        };
        result.elapsed = started.elapsed();
        Ok(result)
    }

    /// The demo has no SQL engine, so the filter runs in memory over the
    /// whole demo table, then the page window applies.
    fn query_table(
        &mut self,
        owner: &str,
        table: &str,
        filter: &TableFilter,
        page: TablePage,
    ) -> Result<QueryResult, DbError> {
        let started = Instant::now();
        let sql =
            crate::filter::select_sql(self.dialect(), owner, table, &TableFilter::default(), page)
                .sql;
        let all = self.query(&sql, FetchLimits::unlimited_bytes(usize::MAX))?;
        let mut columns = all.columns;
        let mut rows: Vec<(usize, Vec<Value>)> = all.rows.into_iter().enumerate().collect();
        for change in self.committed_rows.iter().chain(&self.staged_rows) {
            match change {
                DemoRow::Insert {
                    table: target,
                    index,
                    fields,
                } if target.eq_ignore_ascii_case(table) => {
                    let row = columns
                        .iter()
                        .map(|c| {
                            fields
                                .iter()
                                .find(|(name, _)| *name == c.name)
                                .map_or(Value::Null, |(_, value)| Value::Text(value.clone()))
                        })
                        .collect();
                    rows.push((*index, row));
                }
                DemoRow::Delete {
                    table: target,
                    index,
                } if target.eq_ignore_ascii_case(table) => rows.retain(|(i, _)| i != index),
                _ => {}
            }
        }
        for edit in self.committed.iter().chain(&self.staged) {
            if !edit.table.eq_ignore_ascii_case(table) {
                continue;
            }
            let col = columns.iter().position(|c| c.name == edit.column);
            if let (Some(col), Some((_, row))) =
                (col, rows.iter_mut().find(|(ix, _)| *ix == edit.row))
            {
                row[col] = edit.value.clone().map_or(Value::Null, Value::Text);
            }
        }
        let rows: Vec<Vec<Value>> = rows
            .into_iter()
            .filter(|(_, row)| filter.matches(&columns, row))
            .skip(page.offset)
            .map(|(ix, mut row)| {
                if page.rowid {
                    row.insert(0, Value::Text(demo_rowid(table, ix)));
                }
                row
            })
            .collect();
        if page.rowid {
            columns.insert(
                0,
                ColumnInfo {
                    name: crate::ROWID_COLUMN.to_string(),
                    data_type: "ROWID".to_string(),
                },
            );
        }
        let mut result = finish(columns, rows, FetchLimits::rows(page.limit));
        result.elapsed = started.elapsed();
        Ok(result)
    }

    fn execute(&mut self, sql: &str) -> Result<u64, DbError> {
        self.require_connected()?;
        let kind = crate::sql::statement_kind(sql);
        match kind {
            crate::sql::StatementKind::Select => {
                Err(DbError::new("demo: use query() for SELECT statements"))
            }
            crate::sql::StatementKind::Empty => Err(DbError::new("demo: empty statement")),
            _ => Ok(1),
        }
    }

    fn update_cell(
        &mut self,
        _owner: &str,
        table: &str,
        column: &str,
        rowid: &str,
        value: Option<&str>,
    ) -> Result<(), DbError> {
        self.require_connected()?;
        let prefix = format!("DEMO-{}-", table.to_ascii_uppercase());
        let row: usize = rowid
            .strip_prefix(&prefix)
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| DbError::new("Row no longer exists"))?;
        self.staged.push(DemoEdit {
            table: table.to_string(),
            row,
            column: column.to_string(),
            value: value.map(str::to_string),
        });
        Ok(())
    }

    fn delete_grid_row(&mut self, owner: &str, table: &str, rowid: &str) -> Result<(), DbError> {
        let page = TablePage {
            rowid: true,
            limit: usize::MAX,
            ..TablePage::default()
        };
        let result = self.query_table(owner, table, &TableFilter::default(), page)?;
        if !result.rows.iter().any(|row| row[0].display() == rowid) {
            return Err(DbError::new("Unknown ROWID"));
        }
        let index = rowid
            .rsplit('-')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| DbError::new("Invalid ROWID"))?;
        self.staged_rows.push(DemoRow::Delete {
            table: table.into(),
            index,
        });
        Ok(())
    }

    fn insert_grid_row(
        &mut self,
        _: &str,
        table: &str,
        fields: &[(String, String)],
    ) -> Result<(), DbError> {
        self.require_connected()?;
        if fields.is_empty() {
            return Err(DbError::new("Enter at least one column value"));
        }
        self.staged_rows.push(DemoRow::Insert {
            table: table.into(),
            index: self.next_row,
            fields: fields.to_vec(),
        });
        self.next_row += 1;
        Ok(())
    }

    fn commit(&mut self) -> Result<(), DbError> {
        self.require_connected()?;
        let staged = std::mem::take(&mut self.staged);
        self.committed.extend(staged);
        self.committed_rows.append(&mut self.staged_rows);
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), DbError> {
        self.require_connected()?;
        self.staged_rows.clear();
        self.staged.clear();
        Ok(())
    }

    fn list_objects(&mut self, owner: &str) -> Result<Vec<SchemaObject>, DbError> {
        self.require_connected()?;
        let owner = owner.trim().to_ascii_uppercase();
        let catalog = [
            ("EMPLOYEES", ObjectKind::Table),
            ("DEPARTMENTS", ObjectKind::Table),
            ("EMP_DETAILS_VIEW", ObjectKind::View),
            ("EMP_SUMMARY_MV", ObjectKind::MaterializedView),
            ("EMP_EMP_ID_PK", ObjectKind::Index),
            ("EMP_EMAIL_UK", ObjectKind::Index),
            ("DEPT_LOCATION_IX", ObjectKind::Index),
            ("EMP_MGMT", ObjectKind::Package),
            ("GIVE_RAISE", ObjectKind::Procedure),
            ("GET_SAL", ObjectKind::Function),
            ("EMP_SEQ", ObjectKind::Sequence),
            ("EMP_SYN", ObjectKind::Synonym),
            ("EMP_SAL_TRG", ObjectKind::Trigger),
            ("EMP_ADDRESS_T", ObjectKind::Type),
            ("REMOTE_HR", ObjectKind::DbLink),
        ];
        Ok(catalog
            .into_iter()
            .map(|(name, kind)| SchemaObject {
                owner: owner.clone(),
                name: name.to_string(),
                kind,
            })
            .collect())
    }

    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        self.require_connected()?;
        let mut schemas = vec![
            "HR".to_string(),
            "OE".to_string(),
            "SCOTT".to_string(),
            "SYSTEM".to_string(),
        ];
        if let Some(user) = &self.connected_user {
            if !schemas.contains(user) {
                schemas.push(user.clone());
            }
        }
        schemas.sort();
        Ok(schemas)
    }

    fn object_ddl(
        &mut self,
        owner: &str,
        name: &str,
        kind: &ObjectKind,
    ) -> Result<String, DbError> {
        self.require_connected()?;
        let owner = owner.trim().to_ascii_uppercase();
        let name = name.trim().to_ascii_uppercase();
        let q = |s: &str| format!("\"{s}\"");
        match kind {
            ObjectKind::Table => {
                let cols = self.describe_table(&owner, &name)?;
                let body = cols
                    .iter()
                    .map(|c| {
                        let null = if c.nullable { "" } else { " NOT NULL ENABLE" };
                        format!("    {} {}{null}", q(&c.name), c.data_type)
                    })
                    .collect::<Vec<_>>()
                    .join(",\n");
                Ok(format!(
                    "CREATE TABLE {}.{}\n(\n{body}\n)\nSEGMENT CREATION IMMEDIATE\nTABLESPACE \"USERS\";",
                    q(&owner),
                    q(&name)
                ))
            }
            ObjectKind::View => Ok(format!(
                "CREATE OR REPLACE FORCE VIEW {}.{} AS\nSELECT e.employee_id, e.first_name, e.last_name, d.department_name\n  FROM employees e\n  JOIN departments d ON d.department_id = e.department_id;",
                q(&owner),
                q(&name)
            )),
            ObjectKind::Sequence => Ok(format!(
                "CREATE SEQUENCE {}.{} MINVALUE 1 MAXVALUE 9999999999 INCREMENT BY 1 START WITH 300 CACHE 20 NOORDER NOCYCLE;",
                q(&owner),
                q(&name)
            )),
            ObjectKind::Index => Ok(format!(
                "CREATE UNIQUE INDEX {}.{} ON {}.\"EMPLOYEES\" (\"EMPLOYEE_ID\")\nTABLESPACE \"USERS\";",
                q(&owner),
                q(&name),
                q(&owner)
            )),
            other => Ok(format!(
                "-- demo: DDL for {} {owner}.{name}\nCREATE OR REPLACE {} {}.{} AS\nBEGIN\n  NULL;\nEND;\n/",
                other.group_label(),
                other.oracle_type(),
                q(&owner),
                q(&name)
            )),
        }
    }

    fn describe_table(&mut self, _owner: &str, name: &str) -> Result<Vec<ColumnDef>, DbError> {
        self.require_connected()?;
        let name = name.trim().to_ascii_uppercase();
        let cols: &[(&str, &str, bool)] = match name.as_str() {
            "EMPLOYEES" => &[
                ("EMPLOYEE_ID", "NUMBER(6)", false),
                ("FIRST_NAME", "VARCHAR2(20)", true),
                ("LAST_NAME", "VARCHAR2(25)", false),
                ("EMAIL", "VARCHAR2(25)", false),
                ("HIRE_DATE", "DATE", false),
                ("SALARY", "NUMBER(8,2)", true),
                ("COMMISSION_PCT", "NUMBER(2,2)", true),
            ],
            "DEPARTMENTS" => &[
                ("DEPARTMENT_ID", "NUMBER(4)", false),
                ("DEPARTMENT_NAME", "VARCHAR2(30)", false),
                ("LOCATION_ID", "NUMBER(4)", true),
            ],
            "EMP_DETAILS_VIEW" => &[
                ("EMPLOYEE_ID", "NUMBER(6)", false),
                ("FIRST_NAME", "VARCHAR2(20)", true),
                ("LAST_NAME", "VARCHAR2(25)", false),
                ("DEPARTMENT_NAME", "VARCHAR2(30)", true),
            ],
            _ => return Err(DbError::new(format!("demo: no such table or view: {name}"))),
        };
        Ok(cols
            .iter()
            .map(|(name, data_type, nullable)| ColumnDef {
                name: name.to_string(),
                data_type: data_type.to_string(),
                nullable: *nullable,
                default: None,
            })
            .collect())
    }

    fn list_indexes(&mut self, _owner: &str, table: &str) -> Result<Vec<IndexDef>, DbError> {
        self.require_connected()?;
        let index = |name: &str, unique: bool, columns: &[&str]| IndexDef {
            name: name.to_string(),
            unique,
            index_type: "NORMAL".to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            status: "VALID".to_string(),
        };
        Ok(match table.trim().to_ascii_uppercase().as_str() {
            "EMPLOYEES" => vec![
                index("EMP_EMAIL_UK", true, &["EMAIL"]),
                index("EMP_EMP_ID_PK", true, &["EMPLOYEE_ID"]),
                index("EMP_NAME_IX", false, &["LAST_NAME", "FIRST_NAME"]),
            ],
            "DEPARTMENTS" => vec![index("DEPT_LOCATION_IX", false, &["LOCATION_ID"])],
            _ => Vec::new(),
        })
    }

    fn explain_plan(&mut self, sql: &str) -> Result<Vec<String>, DbError> {
        self.require_connected()?;
        let _ = sql;
        Ok(vec![
            "Plan hash value: 1234567890 (demo plan)".to_string(),
            String::new(),
            "--------------------------------------------------------------------------------"
                .to_string(),
            "| Id  | Operation         | Name      | Rows  | Bytes | Cost (%CPU)| Time     |"
                .to_string(),
            "--------------------------------------------------------------------------------"
                .to_string(),
            "|   0 | SELECT STATEMENT  |           |     6 |   402 |     3   (0)| 00:00:01 |"
                .to_string(),
            "|   1 |  TABLE ACCESS FULL| EMPLOYEES |     6 |   402 |     3   (0)| 00:00:01 |"
                .to_string(),
            "--------------------------------------------------------------------------------"
                .to_string(),
        ])
    }

    fn explain_plan_nodes(&mut self, sql: &str) -> Result<Vec<PlanNode>, DbError> {
        self.require_connected()?;
        let _ = sql;
        Ok(vec![
            PlanNode {
                id: 0,
                parent_id: None,
                operation: "SELECT STATEMENT".into(),
                options: String::new(),
                object_name: String::new(),
                cost: Some(12),
                cardinality: Some(7),
            },
            PlanNode {
                id: 1,
                parent_id: Some(0),
                operation: "NESTED LOOPS".into(),
                options: String::new(),
                object_name: String::new(),
                cost: Some(12),
                cardinality: Some(7),
            },
            PlanNode {
                id: 2,
                parent_id: Some(1),
                operation: "TABLE ACCESS".into(),
                options: "FULL".into(),
                object_name: "EMPLOYEES".into(),
                cost: Some(7),
                cardinality: Some(7),
            },
            PlanNode {
                id: 3,
                parent_id: Some(1),
                operation: "TABLE ACCESS".into(),
                options: "BY INDEX ROWID".into(),
                object_name: "DEPARTMENTS".into(),
                cost: Some(5),
                cardinality: Some(1),
            },
            PlanNode {
                id: 4,
                parent_id: Some(3),
                operation: "INDEX".into(),
                options: "UNIQUE SCAN".into(),
                object_name: "PK_DEPARTMENTS".into(),
                cost: Some(1),
                cardinality: Some(1),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> DemoClient {
        let mut client = DemoClient::new();
        client
            .connect(
                &ConnectionProfile {
                    kind: Default::default(),
                    name: "demo".into(),
                    username: "demo".into(),
                    connect_string: "demo".into(),
                    access: Default::default(),
                },
                "",
            )
            .unwrap();
        client
    }

    #[test]
    fn demo_query_returns_employees_and_honors_cap() {
        let mut client = connected();
        let full = client
            .query("SELECT * FROM employees", FetchLimits::rows(100))
            .unwrap();
        assert_eq!(full.row_count(), 6);
        assert!(!full.truncated);
        assert_eq!(full.columns[0].name, "EMPLOYEE_ID");
        assert_eq!(full.get(0, 0), Some(&Value::Integer(100)));
        assert_eq!(full.get(0, 6), Some(&Value::Null));

        let capped = client
            .query("SELECT * FROM employees", FetchLimits::rows(2))
            .unwrap();
        assert_eq!(capped.row_count(), 2);
        assert!(capped.truncated);
    }

    #[test]
    fn demo_query_accepts_quoted_owner_qualified_names() {
        let mut client = connected();
        let r = client
            .query(
                "SELECT * FROM \"HR\".\"DEPARTMENTS\" FETCH FIRST 1000 ROWS ONLY",
                FetchLimits::rows(100),
            )
            .unwrap();
        assert_eq!(r.columns[0].name, "DEPARTMENT_ID");
    }

    #[test]
    fn demo_schemas_and_ddl() {
        let mut client = connected();
        let schemas = client.list_schemas().unwrap();
        assert!(schemas.contains(&"HR".to_string()));
        assert!(schemas.windows(2).all(|w| w[0] <= w[1]));
        let ddl = client
            .object_ddl("hr", "employees", &ObjectKind::Table)
            .unwrap();
        assert!(ddl.starts_with("CREATE TABLE \"HR\".\"EMPLOYEES\""));
        assert!(ddl.contains("\"EMPLOYEE_ID\" NUMBER(6) NOT NULL ENABLE"));
        assert!(client
            .object_ddl("HR", "GIVE_RAISE", &ObjectKind::Procedure)
            .unwrap()
            .contains("PROCEDURE"));
    }

    #[test]
    fn grid_row_changes_follow_transactions() {
        let mut client = connected();
        let page = TablePage {
            rowid: true,
            ..TablePage::default()
        };
        let read = |client: &mut DemoClient| {
            client
                .query_table("HR", "EMPLOYEES", &TableFilter::default(), page)
                .unwrap()
        };
        let original = read(&mut client);
        let id = original.rows[0][0].display();
        client.delete_grid_row("HR", "EMPLOYEES", &id).unwrap();
        assert!(client.delete_grid_row("HR", "EMPLOYEES", &id).is_err());
        assert_eq!(read(&mut client).row_count() + 1, original.row_count());
        client.rollback().unwrap();
        assert_eq!(read(&mut client).row_count(), original.row_count());
        client
            .insert_grid_row("HR", "EMPLOYEES", &[("FIRST_NAME".into(), "New".into())])
            .unwrap();
        client.commit().unwrap();
        assert_eq!(read(&mut client).row_count(), original.row_count() + 1);
    }

    #[test]
    fn demo_insert_rows_counts_rows() {
        let mut client = connected();
        let n = client
            .insert_rows(
                "HR",
                "DEPARTMENTS",
                &["DEPARTMENT_ID".into()],
                &[vec!["50".into()], vec!["60".into()]],
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn demo_cell_edits_follow_commit_and_rollback() {
        let page = TablePage::new(0, 100).with_rowid(true);
        let filter = TableFilter::default();
        let first_name = |client: &mut DemoClient| {
            let r = client
                .query_table("HR", "EMPLOYEES", &filter, page)
                .unwrap();
            assert_eq!(r.columns[0].name, crate::ROWID_COLUMN);
            let col = r
                .columns
                .iter()
                .position(|c| c.name == "FIRST_NAME")
                .unwrap();
            r.rows[0][col].display()
        };
        let mut client = connected();
        let r = client
            .query_table("HR", "EMPLOYEES", &filter, page)
            .unwrap();
        let rowid = r.rows[0][0].display();
        let original = first_name(&mut client);

        client
            .update_cell("HR", "EMPLOYEES", "FIRST_NAME", &rowid, Some("Zed"))
            .unwrap();
        assert_eq!(first_name(&mut client), "Zed");
        client.rollback().unwrap();
        assert_eq!(first_name(&mut client), original);

        client
            .update_cell("HR", "EMPLOYEES", "FIRST_NAME", &rowid, None)
            .unwrap();
        client.commit().unwrap();
        client.rollback().unwrap();
        assert_eq!(first_name(&mut client), "");
        assert!(client
            .update_cell("HR", "EMPLOYEES", "FIRST_NAME", "bogus", Some("x"))
            .is_err());
    }

    #[test]
    fn demo_query_table_applies_filter_before_cap() {
        use crate::filter::{FilterCondition, FilterJoin, FilterOp};
        let mut client = connected();
        let filter = TableFilter::new(
            vec![FilterCondition::new("SALARY", FilterOp::Ge, "14000")],
            FilterJoin::And,
        );
        let all = client
            .query_table("HR", "EMPLOYEES", &filter, TablePage::new(0, 100))
            .unwrap();
        assert_eq!(all.row_count(), 4);
        assert!(!all.truncated);
        let capped = client
            .query_table("HR", "EMPLOYEES", &filter, TablePage::new(0, 2))
            .unwrap();
        assert_eq!(capped.row_count(), 2);
        assert!(capped.truncated);
        // Second page: the offset skips filtered rows, and the last page
        // is not flagged as truncated.
        let second = client
            .query_table("HR", "EMPLOYEES", &filter, TablePage::new(2, 2))
            .unwrap();
        assert_eq!(second.row_count(), 2);
        assert!(!second.truncated);
        assert_eq!(second.rows[0][0], Value::Integer(102));
        let past_end = client
            .query_table("HR", "EMPLOYEES", &filter, TablePage::new(10, 2))
            .unwrap();
        assert_eq!(past_end.row_count(), 0);
        let none = TableFilter::default();
        assert_eq!(
            client
                .query_table("HR", "EMPLOYEES", &none, TablePage::new(0, 100))
                .unwrap()
                .row_count(),
            6
        );
        // Binds without a SQL engine are refused, not silently ignored.
        assert!(client
            .query_binds(
                "SELECT * FROM employees WHERE x = :1",
                &["1".into()],
                FetchLimits::rows(10)
            )
            .is_err());
    }

    #[test]
    fn demo_lists_indexes_per_table() {
        let mut client = connected();
        let indexes = client.list_indexes("HR", "employees").unwrap();
        assert_eq!(indexes.len(), 3);
        assert!(indexes[0].unique);
        assert_eq!(indexes[2].columns, vec!["LAST_NAME", "FIRST_NAME"]);
        assert!(client
            .list_indexes("HR", "EMP_DETAILS_VIEW")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn demo_query_rejects_unknown_tables() {
        let mut client = connected();
        let err = client
            .query("SELECT * FROM nope", FetchLimits::rows(10))
            .unwrap_err();
        assert!(err.0.contains("EMPLOYEES"), "{err}");
    }

    #[test]
    fn demo_requires_connection() {
        let mut client = DemoClient::new();
        assert!(client
            .query("SELECT * FROM dual", FetchLimits::rows(10))
            .is_err());
        assert!(!client.is_connected());
    }

    #[test]
    fn demo_execute_commit_rollback_cycle() {
        let mut client = connected();
        assert_eq!(
            client.execute("UPDATE employees SET salary = 1").unwrap(),
            1
        );
        assert!(client.execute("SELECT * FROM employees").is_err());
        assert!(client.execute("").is_err());
        assert!(client.commit().is_ok());
        assert!(client.rollback().is_ok());
        client.disconnect();
        assert!(!client.is_connected());
        assert!(client.commit().is_err());
        assert!(client.execute("UPDATE employees SET salary = 1").is_err());
    }

    #[test]
    fn demo_list_objects_and_describe() {
        let mut client = connected();
        let objects = client.list_objects("hr").unwrap();
        assert_eq!(objects.len(), 15);
        assert!(objects.iter().all(|o| o.owner == "HR"));
        // Every group the schema tree shows is represented.
        for kind in [
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::MaterializedView,
            ObjectKind::Index,
            ObjectKind::Package,
            ObjectKind::Procedure,
            ObjectKind::Function,
            ObjectKind::Sequence,
            ObjectKind::Synonym,
            ObjectKind::Trigger,
            ObjectKind::Type,
            ObjectKind::DbLink,
        ] {
            assert!(
                objects.iter().any(|o| o.kind == kind),
                "catalog missing {kind:?}"
            );
        }
        let cols = client.describe_table("HR", "employees").unwrap();
        assert_eq!(cols.len(), 7);
        assert_eq!(cols[0].name, "EMPLOYEE_ID");
        assert!(!cols[0].nullable);
        assert!(cols[1].nullable);
        let err = client.describe_table("HR", "nope").unwrap_err();
        assert!(err.0.contains("NOPE"), "{err}");
    }

    #[test]
    fn demo_connect_requires_username_and_uppercases() {
        let mut client = DemoClient::new();
        let err = client
            .connect(
                &ConnectionProfile {
                    kind: Default::default(),
                    name: "demo".into(),
                    username: "  ".into(),
                    connect_string: "demo".into(),
                    access: Default::default(),
                },
                "",
            )
            .unwrap_err();
        assert!(err.0.contains("username"), "{err}");
        let info = client
            .connect(
                &ConnectionProfile {
                    kind: Default::default(),
                    name: "demo".into(),
                    username: "demo".into(),
                    connect_string: "demo".into(),
                    access: Default::default(),
                },
                "",
            )
            .unwrap();
        assert_eq!(info.user, "DEMO");
        assert_eq!(info.database, "DEMO");
    }

    #[test]
    fn demo_explain_returns_plan_lines() {
        let mut client = connected();
        let plan = client.explain_plan("SELECT * FROM employees").unwrap();
        assert!(!plan.is_empty());
        assert!(plan[0].contains("Plan hash"), "{}", plan[0]);
    }

    #[test]
    fn demo_explain_nodes_form_a_tree() {
        let mut client = connected();
        let nodes = client
            .explain_plan_nodes("SELECT * FROM employees")
            .unwrap();
        assert_eq!(nodes.len(), 5);
        let root = &nodes[0];
        assert_eq!(root.parent_id, None);
        assert_eq!(root.label(), "SELECT STATEMENT");
        assert_eq!(nodes[2].label(), "TABLE ACCESS FULL EMPLOYEES");
        assert_eq!(nodes[2].cost, Some(7));
        // Every non-root node points at an existing parent.
        for node in &nodes[1..] {
            let parent = node.parent_id.expect("non-root has a parent");
            assert!(nodes.iter().any(|n| n.id == parent));
        }
    }
}
