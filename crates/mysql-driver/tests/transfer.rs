//! Data Transfer between MySQL and itself and SQLite. Needs a server:
//! `KILAT_TEST_MYSQL=host:port/database;user;password` (skipped without).

use std::sync::atomic::AtomicBool;

use mysql_driver::MysqlClient;
use ora_core::transfer::run::ClientIo;
use ora_core::transfer::{run_transfer, TableSpec, TransferOptions, TransferSpec};
use ora_core::{ConnectionProfile, DbClient, DbKind, FetchLimits, Value};
use sqlite_driver::SqliteClient;

fn my() -> Option<MysqlClient> {
    let spec = std::env::var("KILAT_TEST_MYSQL").ok()?;
    let mut parts = spec.split(';');
    let (target, user, password) = (parts.next()?, parts.next()?, parts.next()?);
    let mut client = MysqlClient::new();
    client
        .connect(
            &ConnectionProfile {
                kind: DbKind::Mysql,
                username: user.into(),
                connect_string: target.into(),
                ..Default::default()
            },
            password,
        )
        .unwrap();
    Some(client)
}

fn sqlite(tag: &str) -> (SqliteClient, std::path::PathBuf) {
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    let path = std::env::temp_dir().join(format!("my-transfer-{tag}-{}.db", std::process::id()));
    std::fs::File::create(&path).unwrap();
    let mut client = SqliteClient::new();
    client
        .connect(
            &ConnectionProfile {
                kind: DbKind::Sqlite,
                connect_string: path.to_string_lossy().into_owned(),
                ..Default::default()
            },
            "",
        )
        .unwrap();
    (client, path)
}

fn rows(db: &mut dyn DbClient, sql: &str) -> Vec<Vec<String>> {
    db.query(sql, FetchLimits::grid())
        .unwrap()
        .rows
        .iter()
        .map(|r| r.iter().map(Value::display).collect())
        .collect()
}

fn spec(source: &str, target: &str, tables: &[&str]) -> TransferSpec {
    TransferSpec {
        source_schema: source.into(),
        target_schema: target.into(),
        tables: tables.iter().map(|t| TableSpec::auto(*t)).collect(),
        options: TransferOptions::default(),
    }
}

#[test]
fn mysql_to_mysql_to_and_from_sqlite() {
    let Some(mut db) = my() else {
        eprintln!("KILAT_TEST_MYSQL not set: skipped");
        return;
    };
    for sql in [
        "DROP DATABASE IF EXISTS kt_src",
        "DROP DATABASE IF EXISTS kt_dst",
        "CREATE DATABASE kt_src",
        "CREATE DATABASE kt_dst",
        "CREATE TABLE kt_src.dept (id INT PRIMARY KEY, name VARCHAR(50) NOT NULL)",
        "CREATE TABLE kt_src.emp (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20) NOT NULL, \
         salary DECIMAL(10,2), hired DATE, ts DATETIME, note TEXT, data BLOB, \
         flag TINYINT(1), dept INT, UNIQUE KEY emp_name (name), \
         CONSTRAINT emp_dept FOREIGN KEY (dept) REFERENCES kt_src.dept(id), \
         CONSTRAINT pos CHECK (salary >= 0))",
        "INSERT INTO kt_src.dept VALUES (1, 'Ops')",
        "INSERT INTO kt_src.emp (name, salary, hired, ts, note, data, flag, dept) VALUES \
         ('Ana', 1234.50, '2024-01-31', '2024-01-31 10:00:00', 'it''s; \\\\ fine', X'DEADBEEF', 1, 1), \
         ('Bob', NULL, NULL, NULL, NULL, NULL, 0, NULL)",
    ] {
        db.execute(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    db.commit().unwrap();

    // MySQL to MySQL, another database.
    let mut events = Vec::new();
    let mut dst = my().unwrap();
    let summary = run_transfer(
        &mut ClientIo(&mut db),
        &mut ClientIo(&mut dst),
        &spec("kt_src", "kt_dst", &["dept", "emp"]),
        &AtomicBool::new(false),
        &mut |e| events.push(e),
    );
    assert!(summary.ok(), "{summary:?} {events:?}");
    assert_eq!(summary.rows(), 3);
    let copied = rows(
        &mut db,
        "SELECT id, name, salary, hired, note, flag, dept, HEX(data) FROM kt_dst.emp ORDER BY id",
    );
    assert_eq!(
        copied[0],
        [
            "1",
            "Ana",
            "1234.50",
            "2024-01-31",
            "it's; \\ fine",
            "1",
            "1",
            "DEADBEEF"
        ]
    );
    assert_eq!(copied[1][2], "");
    // The unique index came along.
    assert!(db
        .execute("INSERT INTO kt_dst.emp (name) VALUES ('Ana')")
        .is_err());
    db.rollback().unwrap();

    // MySQL to SQLite.
    let (mut lite, path) = sqlite("to-lite");
    let summary = run_transfer(
        &mut ClientIo(&mut db),
        &mut ClientIo(&mut lite),
        &spec("kt_src", "main", &["emp"]),
        &AtomicBool::new(false),
        &mut |e| events.push(e),
    );
    assert!(summary.ok(), "{summary:?} {events:?}");
    assert_eq!(
        rows(&mut lite, "SELECT name, salary, note FROM emp ORDER BY id")[0],
        ["Ana", "1234.5", "it's; \\ fine"]
    );

    // SQLite to MySQL.
    let mut back = spec("main", "kt_dst", &["emp"]);
    back.options.drop_target = true;
    let summary = run_transfer(
        &mut ClientIo(&mut lite),
        &mut ClientIo(&mut db),
        &back,
        &AtomicBool::new(false),
        &mut |e| events.push(e),
    );
    assert!(summary.ok(), "{summary:?} {events:?}");
    let back_rows = rows(
        &mut db,
        "SELECT name, CAST(salary AS CHAR), note FROM kt_dst.emp ORDER BY id",
    );
    assert_eq!(back_rows[0][0], "Ana");
    assert_eq!(back_rows[0][2], "it's; \\ fine");
    assert_eq!(back_rows.len(), 2);
    let _ = std::fs::remove_file(path);

    db.execute("DROP DATABASE kt_src").unwrap();
    db.execute("DROP DATABASE kt_dst").unwrap();
    db.commit().unwrap();
}
