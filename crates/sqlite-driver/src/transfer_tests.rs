//! Data Transfer end to end, SQLite to SQLite (and to an SQL script).

use std::sync::atomic::{AtomicBool, Ordering};

use ora_core::transfer::run::ClientIo;
use ora_core::transfer::{
    run_transfer, Event, FieldMap, Fields, LogLevel, Rows, ScriptIo, TableSpec, TransferOptions,
    TransferSpec, TransferSummary,
};
use ora_core::{ConnectionAccess, ConnectionProfile, DbClient, DbKind, FetchLimits, Value};

use super::SqliteClient;

struct Db {
    client: SqliteClient,
    path: std::path::PathBuf,
}

impl Db {
    fn new(tag: &str, setup: &str) -> Self {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let path = std::env::temp_dir().join(format!(
            "sqlite-transfer-{tag}-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(setup)
            .unwrap();
        let mut client = SqliteClient::new();
        client
            .connect(
                &ConnectionProfile {
                    name: tag.into(),
                    kind: DbKind::Sqlite,
                    username: String::new(),
                    connect_string: path.to_string_lossy().into_owned(),
                    access: ConnectionAccess::default(),
                },
                "",
            )
            .unwrap();
        Db { client, path }
    }

    fn rows(&mut self, sql: &str) -> Vec<Vec<String>> {
        self.client
            .query(sql, FetchLimits::grid())
            .unwrap()
            .rows
            .iter()
            .map(|r| r.iter().map(Value::display).collect())
            .collect()
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

const SOURCE: &str = "
    CREATE TABLE emp (id INTEGER PRIMARY KEY, name TEXT NOT NULL, salary REAL, note TEXT);
    CREATE UNIQUE INDEX emp_name ON emp(name);
    INSERT INTO emp VALUES (1,'Ana',50,'it''s fine'), (2,'Budi',150.5,NULL), (3,'Cici',NULL,'a;b'),
                           (4,'Dedi',10,'x'), (5,'Eka',20,'y');
    CREATE TABLE kv (k TEXT PRIMARY KEY, v BLOB) WITHOUT ROWID;
    INSERT INTO kv VALUES ('a', x'00ff'), ('b', NULL);
    CREATE TABLE \"we\"\"ird\" (\"a b\" TEXT);
    INSERT INTO \"we\"\"ird\" VALUES ('quoted');
    CREATE VIEW rich AS SELECT * FROM emp WHERE salary > 100;
";

fn spec(tables: Vec<TableSpec>, options: TransferOptions) -> TransferSpec {
    TransferSpec {
        source_schema: "main".into(),
        target_schema: "main".into(),
        tables,
        options,
    }
}

fn run(
    src: &mut Db,
    dst: &mut Db,
    spec: &TransferSpec,
    cancel: &AtomicBool,
) -> (TransferSummary, Vec<Event>) {
    let mut events = Vec::new();
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut ClientIo(&mut dst.client),
        spec,
        cancel,
        &mut |e| events.push(e),
    );
    (summary, events)
}

#[test]
fn auto_mode_copies_tables_rows_and_indexes() {
    let mut src = Db::new("auto-src", SOURCE);
    let mut dst = Db::new("auto-dst", "");
    let s = spec(
        vec![
            TableSpec::auto("emp"),
            TableSpec::auto("kv"),
            TableSpec::auto("we\"ird"),
        ],
        TransferOptions::default(),
    );
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(summary.rows(), 8);
    assert_eq!(
        dst.rows("SELECT id, name, salary, note FROM emp ORDER BY id")[0],
        ["1", "Ana", "50", "it's fine"]
    );
    assert_eq!(dst.rows("SELECT note FROM emp WHERE id = 3"), [["a;b"]]);
    assert_eq!(
        dst.rows("SELECT count(*) FROM emp WHERE salary IS NULL"),
        [["1"]]
    );
    // WITHOUT ROWID has no row key: copied through offset paging.
    assert_eq!(dst.rows("SELECT count(*) FROM kv"), [["2"]]);
    assert_eq!(dst.rows("SELECT hex(v) FROM kv WHERE k = 'a'"), [["00FF"]]);
    assert_eq!(dst.rows("SELECT \"a b\" FROM \"we\"\"ird\""), [["quoted"]]);
    // The unique index came along and still enforces uniqueness.
    assert_eq!(
        dst.rows("SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'emp_name'"),
        [["emp_name"]]
    );
    assert!(dst
        .client
        .execute("INSERT INTO emp (id, name) VALUES (99, 'Ana')")
        .is_err());
    // Source untouched.
    assert_eq!(src.rows("SELECT count(*) FROM emp"), [["5"]]);
}

#[test]
fn advanced_mode_renames_maps_fields_and_splits_recordsets() {
    let mut src = Db::new("adv-src", SOURCE);
    let mut dst = Db::new("adv-dst", "");
    let mut t = TableSpec::auto("emp");
    t.target = Some("staff".into());
    t.fields = Fields::Custom(vec![
        FieldMap {
            source: "id".into(),
            target: "staff_id".into(),
        },
        FieldMap {
            source: "name".into(),
            target: "full_name".into(),
        },
    ]);
    let conditions = ora_core::transfer::ranges::generate(
        DbKind::Sqlite,
        "id",
        ora_core::transfer::ranges::RangeKind::Integer,
        "2",
        "4",
        2,
    )
    .unwrap();
    t.rows = Rows::Custom {
        batch: 1,
        conditions,
    };
    let (summary, events) = run(
        &mut src,
        &mut dst,
        &spec(vec![t], TransferOptions::default()),
        &AtomicBool::new(false),
    );
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(
        dst.rows("SELECT staff_id, full_name FROM staff ORDER BY staff_id"),
        [["2", "Budi"], ["3", "Cici"], ["4", "Dedi"]]
    );
    // The index uses a column that was not transferred partly: name is
    // mapped, so it follows the new column name.
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Log(LogLevel::Info, m) if m.contains("recordset 2 of 2"))));
    // Batch of one row: progress events per row.
    let rows_events = events
        .iter()
        .filter(|e| matches!(e, Event::Rows { .. }))
        .count();
    assert_eq!(rows_events, 3);
}

#[test]
fn name_case_conversion_applies_to_tables_and_columns() {
    let mut src = Db::new("case-src", SOURCE);
    let mut dst = Db::new("case-dst", "");
    let mut o = TransferOptions::default();
    o.convert_case = Some(ora_core::transfer::NameCase::Upper);
    let (summary, _) = run(
        &mut src,
        &mut dst,
        &spec(vec![TableSpec::auto("emp")], o),
        &AtomicBool::new(false),
    );
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(dst.rows("SELECT NAME FROM \"EMP\" WHERE ID = 1"), [["Ana"]]);
}

#[test]
fn existing_target_is_kept_unless_dropped_or_emptied() {
    let existing = "CREATE TABLE emp (id INTEGER PRIMARY KEY, name TEXT, salary REAL, note TEXT);
                    INSERT INTO emp VALUES (100, 'Old', 1, 'old');";
    let mut src = Db::new("keep-src", SOURCE);

    // Default: the existing table keeps its rows, new rows are added.
    let mut dst = Db::new("keep-dst", existing);
    let s = spec(vec![TableSpec::auto("emp")], TransferOptions::default());
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert!(!summary.tables[0].created);
    assert_eq!(dst.rows("SELECT count(*) FROM emp"), [["6"]]);

    // Empty target tables first.
    let mut dst = Db::new("empty-dst", existing);
    let mut o = TransferOptions::default();
    o.empty_target = true;
    let (summary, _) = run(
        &mut src,
        &mut dst,
        &spec(vec![TableSpec::auto("emp")], o),
        &AtomicBool::new(false),
    );
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(
        dst.rows("SELECT count(*) FROM emp WHERE name = 'Old'"),
        [["0"]]
    );
    assert_eq!(dst.rows("SELECT count(*) FROM emp"), [["5"]]);

    // Drop and recreate: the old structure is gone too.
    let mut dst = Db::new("drop-dst", "CREATE TABLE emp (only_col TEXT);");
    let mut o = TransferOptions::default();
    o.drop_target = true;
    let (summary, _) = run(
        &mut src,
        &mut dst,
        &spec(vec![TableSpec::auto("emp")], o),
        &AtomicBool::new(false),
    );
    assert!(summary.ok(), "{summary:?}");
    assert!(summary.tables[0].created);
    assert_eq!(dst.rows("SELECT count(*) FROM emp"), [["5"]]);
}

#[test]
fn error_rolls_back_the_table_unless_continue_on_error() {
    // Target already has id 3, so the batch fails on row 3.
    let existing = "CREATE TABLE emp (id INTEGER PRIMARY KEY, name TEXT, salary REAL, note TEXT);
                    INSERT INTO emp VALUES (3, 'Clash', 1, 'c');";
    let mut src = Db::new("err-src", SOURCE);

    let mut dst = Db::new("err-dst", existing);
    let s = spec(vec![TableSpec::auto("emp")], TransferOptions::default());
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert_eq!(summary.failed_tables(), 1);
    let error = summary.tables[0].error.clone().unwrap();
    assert!(error.contains("UNIQUE"), "{error}");
    assert_eq!(dst.rows("SELECT count(*) FROM emp"), [["1"]], "rolled back");

    let mut dst = Db::new("cont-dst", existing);
    let mut o = TransferOptions::default();
    o.continue_on_error = true;
    let (summary, events) = run(
        &mut src,
        &mut dst,
        &spec(vec![TableSpec::auto("emp")], o),
        &AtomicBool::new(false),
    );
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(summary.tables[0].rows, 4);
    assert_eq!(summary.tables[0].skipped, 1);
    assert_eq!(dst.rows("SELECT count(*) FROM emp"), [["5"]]);
    // The log names the problem but never a row value.
    let log: String = events
        .iter()
        .filter_map(|e| match e {
            Event::Log(_, m) => Some(m.clone()),
            _ => None,
        })
        .collect();
    assert!(log.contains("row skipped") && !log.contains("Clash") && !log.contains("Cici"));
}

#[test]
fn stop_between_batches_rolls_the_table_back() {
    let mut src = Db::new("stop-src", SOURCE);
    let mut dst = Db::new("stop-dst", "");
    let mut t = TableSpec::auto("emp");
    t.rows = Rows::All { batch: 2 };
    let s = spec(vec![t, TableSpec::auto("kv")], TransferOptions::default());
    let cancel = AtomicBool::new(false);
    let mut seen = 0;
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut ClientIo(&mut dst.client),
        &s,
        &cancel,
        &mut |e| {
            if matches!(e, Event::Rows { .. }) {
                seen += 1;
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(summary.cancelled && !summary.ok());
    assert_eq!(seen, 1);
    // SQLite rolls the CREATE back with the rows; Oracle would keep the
    // empty table. Either way no row survives.
    let left = dst
        .client
        .query("SELECT count(*) FROM emp", FetchLimits::grid())
        .map(|r| r.rows[0][0].display());
    assert!(left.is_err() || left == Ok("0".to_string()), "{left:?}");
    assert_eq!(summary.tables.len(), 1, "the next table never started");
}

#[test]
fn invalid_spec_runs_nothing() {
    let mut src = Db::new("inv-src", SOURCE);
    let mut dst = Db::new("inv-dst", "");
    let mut t = TableSpec::auto("emp");
    t.rows = Rows::Custom {
        batch: 10,
        conditions: vec!["1=1; DROP TABLE emp".into()],
    };
    let (summary, _) = run(
        &mut src,
        &mut dst,
        &spec(vec![t], TransferOptions::default()),
        &AtomicBool::new(false),
    );
    assert!(summary.invalid.is_some());
    assert!(dst.rows("SELECT name FROM sqlite_master").is_empty());
    assert_eq!(src.rows("SELECT count(*) FROM emp"), [["5"]]);
}

#[test]
fn missing_source_table_fails_that_table_only() {
    let mut src = Db::new("miss-src", SOURCE);
    let mut dst = Db::new("miss-dst", "");
    let mut o = TransferOptions::default();
    o.continue_on_error = true;
    let s = spec(vec![TableSpec::auto("nope"), TableSpec::auto("kv")], o);
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert_eq!(summary.failed_tables(), 1);
    assert_eq!(summary.tables[1].rows, 2);
}

#[test]
fn script_target_writes_statements_without_touching_a_database() {
    let mut src = Db::new("script-src", SOURCE);
    let mut out = ScriptIo::new(DbKind::Oracle, Vec::new());
    let s = spec(vec![TableSpec::auto("emp")], TransferOptions::default());
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut out,
        &s,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(summary.ok(), "{summary:?}");
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("CREATE TABLE \"main\".\"emp\""));
    assert!(text.contains("\"id\" NUMBER(19) NOT NULL") || text.contains("\"id\" NUMBER(19)"));
    assert!(text.contains("INSERT INTO \"main\".\"emp\" (\"id\", \"name\", \"salary\", \"note\") VALUES (1, 'Ana', 50, 'it''s fine');"));
    let commit = text.find("COMMIT;").unwrap();
    let index = text
        .find("CREATE UNIQUE INDEX \"main\".\"emp_name\" ON \"main\".\"emp\"")
        .unwrap();
    assert!(commit < index, "indexes follow the rows");
}
