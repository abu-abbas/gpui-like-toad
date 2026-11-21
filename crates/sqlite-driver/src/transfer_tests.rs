//! Data Transfer end to end, SQLite to SQLite (and to an SQL script).

use std::sync::atomic::{AtomicBool, Ordering};

use ora_core::transfer::run::ClientIo;
use ora_core::transfer::{
    run_transfer, Event, FieldMap, Fields, LogLevel, Rows, ScriptIo, TableSpec, TransferOptions,
    TransferSpec, TransferSummary,
};
use ora_core::ConstraintKind::{Check, ForeignKey, PrimaryKey, Unique};
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

    /// Rows as another connection sees them: only committed work.
    fn committed(&self, sql: &str) -> Vec<Vec<String>> {
        let conn = rusqlite::Connection::open(&self.path).unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            (0..n)
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
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
    // Everything, indexes included, is committed, not just visible to
    // the transferring session.
    assert_eq!(dst.committed("SELECT count(*) FROM emp"), [["Integer(5)"]]);
    assert_eq!(
        dst.committed("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name"),
        // kv's primary key now comes along as a constraint (its own index).
        [["Text(\"emp_name\")"], ["Text(\"sqlite_autoindex_kv_1\")"]]
    );
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
    let o = TransferOptions {
        convert_case: Some(ora_core::transfer::NameCase::Upper),
        ..Default::default()
    };
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
    let o = TransferOptions {
        empty_target: true,
        ..Default::default()
    };
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
    let o = TransferOptions {
        drop_target: true,
        ..Default::default()
    };
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
    let o = TransferOptions {
        continue_on_error: true,
        ..Default::default()
    };
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
    let o = TransferOptions {
        continue_on_error: true,
        ..Default::default()
    };
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

#[test]
fn structure_only_commits_the_new_tables() {
    let mut src = Db::new("struct-src", SOURCE);
    let mut dst = Db::new("struct-dst", "");
    let o = TransferOptions {
        create_records: false,
        ..Default::default()
    };
    let s = spec(vec![TableSpec::auto("emp")], o);
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(dst.committed("SELECT count(*) FROM emp"), [["Integer(0)"]]);
    assert_eq!(
        dst.committed("SELECT count(*) FROM sqlite_master WHERE type = 'index'"),
        [["Integer(1)"]]
    );
}

const KEYED: &str = "
    CREATE TABLE dept (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE, name TEXT DEFAULT 'none');
    CREATE TABLE staff (
        id INTEGER PRIMARY KEY,
        dept_id INTEGER REFERENCES dept(id) ON DELETE CASCADE,
        level INTEGER NOT NULL DEFAULT 1 CHECK (level BETWEEN 1 AND 5),
        mail TEXT,
        CHECK (length(mail) < 20 OR mail IS NULL)
    );
    CREATE INDEX staff_level ON staff(level);
    INSERT INTO dept VALUES (1,'IT','Tech'), (2,'HR',NULL);
    INSERT INTO staff VALUES (1,1,2,'a@b.c'), (2,2,3,NULL);
";

#[test]
fn constraints_and_defaults_are_read_and_copied() {
    let mut src = Db::new("keys-src", KEYED);
    let kinds: Vec<_> = src
        .client
        .list_constraints("main", "staff")
        .unwrap()
        .into_iter()
        .map(|c| (c.kind, c.columns, c.ref_table, c.condition))
        .collect();
    assert!(kinds.contains(&(PrimaryKey, vec!["id".into()], String::new(), String::new())));
    assert!(kinds.contains(&(
        ForeignKey,
        vec!["dept_id".into()],
        "dept".into(),
        String::new()
    )));
    assert!(kinds
        .iter()
        .any(|k| k.0 == Check && k.3 == "level BETWEEN 1 AND 5"));
    assert!(kinds
        .iter()
        .any(|k| k.0 == Check && k.3.starts_with("length(mail)")));

    let mut dst = Db::new("keys-dst", "");
    let s = spec(
        vec![TableSpec::auto("dept"), TableSpec::auto("staff")],
        TransferOptions::default(),
    );
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(summary.rows(), 4);

    let kinds: Vec<_> = dst
        .client
        .list_constraints("main", "staff")
        .unwrap()
        .into_iter()
        .map(|c| c.kind)
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k == Check).count(), 2);
    assert!(kinds.contains(&PrimaryKey) && kinds.contains(&ForeignKey));
    // UNIQUE on dept.code came along as a constraint, not a second index.
    let dept = dst.client.list_constraints("main", "dept").unwrap();
    assert!(dept
        .iter()
        .any(|c| c.kind == Unique && c.columns == ["code"]));
    // Enforced on the target.
    let e = dst
        .client
        .execute("INSERT INTO staff (id, level) VALUES (9, 9)")
        .unwrap_err();
    assert!(e.to_string().to_lowercase().contains("check"), "{e}");
    assert!(dst
        .client
        .execute("INSERT INTO dept (id, code) VALUES (7, 'IT')")
        .is_err());
    dst.client
        .execute("INSERT INTO dept (id, code) VALUES (8, 'XX')")
        .unwrap();
    assert_eq!(dst.rows("SELECT name FROM dept WHERE id = 8"), [["none"]]);
    let cols = dst.client.describe_table("main", "staff").unwrap();
    assert_eq!(cols[2].default.as_deref(), Some("1"));
    // The plain index is still copied.
    assert_eq!(
        dst.rows("SELECT count(*) FROM sqlite_master WHERE name = 'staff_level'"),
        [["1"]]
    );
}

#[test]
fn constraint_options_off_copy_plain_tables() {
    let mut src = Db::new("nokeys-src", KEYED);
    let mut dst = Db::new("nokeys-dst", "");
    let s = spec(
        vec![TableSpec::auto("dept"), TableSpec::auto("staff")],
        TransferOptions {
            include_primary_keys: false,
            include_uniques: false,
            include_checks: false,
            include_foreign_keys: false,
            include_defaults: false,
            ..TransferOptions::default()
        },
    );
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert!(dst
        .client
        .list_constraints("main", "staff")
        .unwrap()
        .is_empty());
    dst.client
        .execute("INSERT INTO staff (id, level) VALUES (9, 9)")
        .unwrap();
    assert_eq!(
        dst.client.describe_table("main", "staff").unwrap()[2].default,
        None
    );
}

#[test]
fn foreign_key_to_a_table_outside_the_transfer_is_skipped_with_a_log() {
    let mut src = Db::new("fkskip-src", KEYED);
    let mut dst = Db::new("fkskip-dst", "");
    let s = spec(vec![TableSpec::auto("staff")], TransferOptions::default());
    let (summary, events) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    assert!(events.iter().any(|e| matches!(e,
        Event::Log(LogLevel::Warn, m) if m.contains("foreign key") && m.contains("not part of this transfer"))));
    assert!(dst
        .client
        .list_constraints("main", "staff")
        .unwrap()
        .iter()
        .all(|c| c.kind != ora_core::ConstraintKind::ForeignKey));
}

#[test]
fn triggers_are_copied_after_the_rows_and_fire_on_the_target() {
    let mut src = Db::new(
        "trg-src",
        "CREATE TABLE a (id INTEGER PRIMARY KEY, n INTEGER);
         CREATE TABLE log (msg TEXT);
         CREATE TRIGGER a_ins AFTER INSERT ON a BEGIN INSERT INTO log VALUES ('ins ' || NEW.id); END;
         INSERT INTO a VALUES (1, 10), (2, 20);",
    );
    let mut dst = Db::new("trg-dst", "");
    let s = spec(
        vec![TableSpec::auto("a"), TableSpec::auto("log")],
        TransferOptions {
            include_triggers: true,
            ..TransferOptions::default()
        },
    );
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok(), "{summary:?}");
    // The load itself did not fire the trigger: the log holds the source's rows.
    assert_eq!(dst.rows("SELECT count(*) FROM log"), [["2"]]);
    dst.client.execute("INSERT INTO a VALUES (3, 30)").unwrap();
    assert_eq!(dst.rows("SELECT count(*) FROM log"), [["3"]]);
}

#[test]
fn triggers_stay_off_unless_asked() {
    let mut src = Db::new(
        "trgoff-src",
        "CREATE TABLE a (id INTEGER PRIMARY KEY);
         CREATE TRIGGER a_ins AFTER INSERT ON a BEGIN SELECT 1; END;",
    );
    let mut dst = Db::new("trgoff-dst", "");
    let s = spec(vec![TableSpec::auto("a")], TransferOptions::default());
    let (summary, _) = run(&mut src, &mut dst, &s, &AtomicBool::new(false));
    assert!(summary.ok());
    assert!(dst.client.trigger_scripts("main", "a").unwrap().is_empty());
}

#[test]
fn csv_target_writes_one_safe_file_per_table_and_refuses_to_overwrite() {
    use ora_core::transfer::CsvIo;
    let mut src = Db::new("csv-src", SOURCE);
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    let dir = std::env::temp_dir().join(format!("transfer-csv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let s = spec(
        vec![TableSpec::auto("emp"), TableSpec::auto("we\"ird")],
        TransferOptions::default(),
    );
    let mut io = CsvIo::new(&dir, false);
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut io,
        &s,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    io.finish().unwrap();
    assert!(summary.ok(), "{summary:?}");
    assert_eq!(summary.rows(), 6);
    let emp = std::fs::read_to_string(dir.join("emp.csv")).unwrap();
    assert!(
        emp.starts_with("id,name,salary,note\r\n1,Ana,50,it's fine\r\n"),
        "{emp}"
    );
    assert!(dir.join("we_ird.csv").exists());

    // A second run does not overwrite...
    let mut io = CsvIo::new(&dir, false);
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut io,
        &s,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(!summary.ok());
    assert!(summary.tables[0]
        .error
        .as_deref()
        .unwrap()
        .contains("already exists"));
    // ...unless asked to.
    let mut io = CsvIo::new(&dir, true);
    let summary = run_transfer(
        &mut ClientIo(&mut src.client),
        &mut io,
        &s,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    io.finish().unwrap();
    assert!(summary.ok(), "{summary:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
