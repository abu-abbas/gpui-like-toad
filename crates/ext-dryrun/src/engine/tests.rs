use super::*;
use ora_core::client::{DbClient, DbError, DbInfo};
use ora_core::model::{ColumnDef, ColumnInfo, ConnectionProfile, ObjectKind, QueryResult};
use ora_core::{DbKind, SchemaObject};

type Row = (String, Vec<Value>);

/// An in-memory table. The statement it "runs" is a closure, and the
/// capture query is answered by a predicate, so the engine's own logic is
/// what the tests exercise.
pub(crate) struct Fake {
    columns: Vec<&'static str>,
    pub(crate) rows: Vec<Row>,
    matching: fn(&[Value]) -> bool,
    statement: fn(&mut Vec<Row>) -> u64,
    pub(crate) triggers: i128,
    pub(crate) queries: Vec<String>,
    pub(crate) rolled_back: usize,
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn int(n: i128) -> Value {
    Value::Integer(n)
}

fn emp() -> Vec<Row> {
    vec![
        ("AAA1".into(), vec![int(1), text("Ann"), int(100), int(10)]),
        ("AAA2".into(), vec![int(2), text("Bob"), int(200), int(10)]),
        (
            "AAA3".into(),
            vec![int(3), text("O'Neil"), int(300), int(20)],
        ),
    ]
}

impl Fake {
    pub(crate) fn new(matching: fn(&[Value]) -> bool, statement: fn(&mut Vec<Row>) -> u64) -> Self {
        Fake {
            columns: vec!["ID", "NAME", "SALARY", "DEPT"],
            rows: emp(),
            matching,
            statement,
            triggers: 0,
            queries: Vec::new(),
            rolled_back: 0,
        }
    }

    fn result(&self, rows: Vec<&Row>) -> QueryResult {
        let mut columns = vec![ColumnInfo {
            name: "KILAT_RID".into(),
            data_type: "ROWID".into(),
        }];
        columns.extend(self.columns.iter().map(|n| ColumnInfo {
            name: (*n).into(),
            data_type: "".into(),
        }));
        QueryResult {
            columns,
            rows: rows
                .into_iter()
                .map(|(id, cells)| {
                    let mut row = vec![text(id)];
                    row.extend(cells.iter().cloned());
                    row
                })
                .collect(),
            ..QueryResult::default()
        }
    }
}

impl DbClient for Fake {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }
    fn connect(&mut self, _: &ConnectionProfile, _: &str) -> Result<DbInfo, DbError> {
        unreachable!()
    }
    fn disconnect(&mut self) {}
    fn is_connected(&self) -> bool {
        true
    }
    fn query(&mut self, sql: &str, limits: FetchLimits) -> Result<QueryResult, DbError> {
        self.queries.push(sql.to_string());
        let mut result = if let Some(at) = sql.find("ROWID IN (") {
            let ids: Vec<&str> = sql[at..].split('\'').skip(1).step_by(2).collect();
            self.result(
                self.rows
                    .iter()
                    .filter(|(id, _)| ids.contains(&id.as_str()))
                    .collect(),
            )
        } else {
            self.result(
                self.rows
                    .iter()
                    .filter(|(_, c)| (self.matching)(c))
                    .collect(),
            )
        };
        if result.rows.len() > limits.max_rows {
            result.rows.truncate(limits.max_rows);
            result.truncated = true;
        }
        Ok(result)
    }
    fn query_binds(
        &mut self,
        _: &str,
        _: &[String],
        _: FetchLimits,
    ) -> Result<QueryResult, DbError> {
        Ok(QueryResult {
            rows: vec![vec![int(self.triggers)]],
            ..QueryResult::default()
        })
    }
    fn execute(&mut self, _: &str) -> Result<u64, DbError> {
        Ok((self.statement)(&mut self.rows))
    }
    fn commit(&mut self) -> Result<(), DbError> {
        Ok(())
    }
    fn rollback(&mut self) -> Result<(), DbError> {
        self.rolled_back += 1;
        Ok(())
    }
    fn list_objects(&mut self, _: &str) -> Result<Vec<SchemaObject>, DbError> {
        unreachable!()
    }
    fn list_schemas(&mut self) -> Result<Vec<String>, DbError> {
        unreachable!()
    }
    fn object_ddl(&mut self, _: &str, _: &str, _: &ObjectKind) -> Result<String, DbError> {
        unreachable!()
    }
    fn describe_table(&mut self, _: &str, _: &str) -> Result<Vec<ColumnDef>, DbError> {
        unreachable!()
    }
    fn explain_plan(&mut self, _: &str) -> Result<Vec<String>, DbError> {
        unreachable!()
    }
}

pub(crate) fn dept10(c: &[Value]) -> bool {
    c[3] == int(10)
}

#[allow(clippy::ptr_arg)]
pub(crate) fn raise(rows: &mut Vec<Row>) -> u64 {
    let mut n = 0;
    for (_, c) in rows.iter_mut().filter(|(_, c)| c[3] == int(10)) {
        if let Value::Integer(s) = c[2] {
            c[2] = int(s + 10);
        }
        n += 1;
    }
    n
}

fn remove(rows: &mut Vec<Row>) -> u64 {
    let before = rows.len();
    rows.retain(|(_, c)| c[3] != int(10));
    (before - rows.len()) as u64
}

#[test]
fn update_reports_old_and_new_and_an_undo_script() {
    let mut db = Fake::new(dept10, raise);
    let report = run(
        &mut db,
        "UPDATE emp e SET salary = salary + 10 WHERE e.dept = 10;",
    )
    .unwrap();
    assert_eq!(report.affected, 2);
    assert_eq!(report.changed_rows, 2);
    assert_eq!(report.columns, ["ID", "NAME", "SALARY", "DEPT"]);
    assert_eq!(report.changes[0].changed, vec![2]);
    assert_eq!(report.changes[0].old[2], int(100));
    assert_eq!(report.changes[0].new.as_ref().unwrap()[2], int(110));
    let undo = report.undo.unwrap();
    assert!(undo.contains("UPDATE emp SET \"SALARY\" = 100 WHERE ROWID = CHARTOROWID('AAA1');"));
    assert!(undo.contains("\"SALARY\" = 200 WHERE ROWID = CHARTOROWID('AAA2');"));
    assert!(!undo.contains("AAA3"));
    // The capture used the statement's own alias and WHERE.
    assert_eq!(
        db.queries[0],
        "SELECT e.ROWID AS KILAT_RID, e.* FROM emp e WHERE e.dept = 10"
    );
    assert!(db.queries[1].contains("e.ROWID IN (CHARTOROWID('AAA1'), CHARTOROWID('AAA2'))"));
}

#[test]
fn rows_the_update_leaves_alone_are_counted_not_listed() {
    let mut db = Fake::new(dept10, |_| 2);
    let report = run(&mut db, "UPDATE emp SET salary = salary WHERE dept = 10").unwrap();
    assert_eq!(report.changed_rows, 0);
    assert_eq!(report.unchanged_rows, 2);
    assert!(report.changes.is_empty());
}

#[test]
fn delete_writes_insert_statements() {
    let mut db = Fake::new(
        |_| true,
        |rows| {
            let n = rows.len() as u64;
            rows.clear();
            n
        },
    );
    let report = run(&mut db, "DELETE FROM emp").unwrap();
    assert_eq!(report.affected, 3);
    assert_eq!(report.changed_rows, 3);
    assert!(report.changes.iter().all(|c| c.new.is_none()));
    let undo = report.undo.unwrap();
    assert!(undo.contains(
        "INSERT INTO emp (\"ID\", \"NAME\", \"SALARY\", \"DEPT\") VALUES (3, 'O''Neil', 300, 20);"
    ));
    assert_eq!(db.queries.len(), 1, "DELETE does not read rows again");
}

#[test]
fn delete_with_where() {
    let mut db = Fake::new(dept10, remove);
    let report = run(&mut db, "DELETE FROM emp WHERE dept = 10").unwrap();
    assert_eq!(report.affected, 2);
    assert_eq!(report.captured, 2);
    assert!(!report.partial);
}

#[test]
fn more_rows_than_the_capture_cap_gives_no_undo_script() {
    let mut db = Fake::new(|_| true, |rows| rows.len() as u64);
    db.rows = (0..CAPTURE_ROWS + 5)
        .map(|n| {
            (
                format!("R{n}"),
                vec![int(n as i128), text("x"), int(1), int(1)],
            )
        })
        .collect();
    let report = run(&mut db, "DELETE FROM emp").unwrap();
    assert!(report.partial);
    assert_eq!(report.captured, CAPTURE_ROWS);
    assert_eq!(report.changes.len(), SHOW_ROWS);
    assert_eq!(report.affected as usize, CAPTURE_ROWS + 5);
    assert!(report.undo.is_none());
    assert!(report.notes[0].contains("no undo script"));
}

#[test]
fn rows_changed_by_someone_else_in_between_give_no_undo_script() {
    // The statement reports 3 affected rows but only 2 were captured.
    let mut db = Fake::new(dept10, |_| 3);
    let report = run(&mut db, "UPDATE emp SET salary = 1 WHERE dept = 10").unwrap();
    assert!(report.partial);
    assert!(report.undo.is_none());
}

#[test]
fn a_clipped_value_is_not_written_into_the_script() {
    let mut db = Fake::new(dept10, remove);
    db.rows[0].1[1] = Value::Clipped(Box::new(ora_core::model::ClippedValue {
        preview: text("aaaa"),
        full_len: 90_000,
        unit: ora_core::model::LenUnit::Chars,
    }));
    let report = run(&mut db, "DELETE FROM emp WHERE dept = 10").unwrap();
    let undo = report.undo.unwrap();
    assert!(undo.contains("-- ROWID AAA1: a value is too large"));
    assert!(!undo.contains("INSERT INTO emp (\"ID\", \"NAME\", \"SALARY\", \"DEPT\") VALUES (1,"));
    assert!(undo.contains("VALUES (2, 'Bob', 200, 10);"));
    assert!(report.notes.iter().any(|n| n.contains("cannot restore")));
}

#[test]
fn trigger_count_comes_through() {
    let mut db = Fake::new(dept10, raise);
    db.triggers = 2;
    let report = run(
        &mut db,
        "UPDATE emp SET salary = salary + 10 WHERE dept = 10",
    )
    .unwrap();
    assert_eq!(report.triggers, Some(2));
}

#[test]
fn refused_statements_never_reach_the_database() {
    let mut db = Fake::new(dept10, raise);
    assert!(run(
        &mut db,
        "MERGE INTO emp USING d ON (1=1) WHEN MATCHED THEN UPDATE SET a=1"
    )
    .is_err());
    assert!(run(&mut db, "DROP TABLE emp").is_err());
    assert!(db.queries.is_empty());
}

#[test]
fn a_script_over_the_view_limit_is_not_written() {
    let mut db = Fake::new(|_| true, |rows| rows.len() as u64);
    let wide = "x".repeat(3000);
    db.rows = (0..40)
        .map(|n| (format!("R{n}"), vec![int(n), text(&wide), int(1), int(1)]))
        .collect();
    let report = run(&mut db, "DELETE FROM emp").unwrap();
    assert!(report.undo.is_none());
    assert!(report.notes.iter().any(|n| n.contains("not written")));
    assert!(!report.partial);
}
