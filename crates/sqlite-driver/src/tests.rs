use std::path::PathBuf;

use ora_core::{
    ConnectionAccess, ConnectionProfile, DbClient, DbKind, FetchLimits, TableFilter, TablePage,
    Value,
};

use super::SqliteClient;

/// Test-only scratch path, unique per process, thread and tag. Nothing
/// secret is written there.
fn scratch_path(tag: &str) -> PathBuf {
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    std::env::temp_dir().join(format!(
        "sqlite-driver-{tag}-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// A fresh database file with a small `emp` table, removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(tag: &str) -> Self {
        let path = scratch_path(tag);
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE emp (id INTEGER PRIMARY KEY, name TEXT NOT NULL, salary REAL DEFAULT 0);
             CREATE INDEX emp_name ON emp(name);
             CREATE VIEW rich AS SELECT * FROM emp WHERE salary > 100;
             CREATE TABLE kv (k TEXT PRIMARY KEY, v BLOB) WITHOUT ROWID;
             INSERT INTO emp (name, salary) VALUES ('Ana', 50), ('Budi', 150.5), ('Cici', NULL);
             INSERT INTO kv VALUES ('a', x'00ff');",
        )
        .unwrap();
        TempDb(path)
    }

    fn profile(&self, read_only: bool) -> ConnectionProfile {
        ConnectionProfile {
            name: "test".into(),
            kind: DbKind::Sqlite,
            username: String::new(),
            connect_string: self.0.to_string_lossy().into_owned(),
            access: ConnectionAccess {
                read_only,
                ..Default::default()
            },
        }
    }

    fn client(&self) -> SqliteClient {
        let mut client = SqliteClient::new();
        client.connect(&self.profile(false), "").unwrap();
        client
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn missing_file_is_an_error_and_is_not_created() {
    let mut client = SqliteClient::new();
    let path = scratch_path("does-not-exist");
    let err = client
        .connect(
            &ConnectionProfile {
                kind: DbKind::Sqlite,
                connect_string: path.to_string_lossy().into_owned(),
                ..Default::default()
            },
            "",
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("SQLite database file not found"),
        "{err}"
    );
    assert!(!path.exists());
}

#[test]
fn query_maps_sqlite_types_and_caps_rows() {
    let db = TempDb::new("types");
    let mut client = db.client();
    let result = client
        .query(
            "SELECT id, name, salary FROM emp ORDER BY id;",
            FetchLimits::rows(2),
        )
        .unwrap();
    assert_eq!(result.columns[0].data_type, "INTEGER");
    assert_eq!(result.rows.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.rows[0][0], Value::Integer(1));
    assert_eq!(result.rows[1][2], Value::Float(150.5));
    let kv = client
        .query("SELECT v FROM kv", FetchLimits::rows(10))
        .unwrap();
    assert_eq!(kv.rows[0][0], Value::Bytes(vec![0, 255]));
}

#[test]
fn changes_wait_for_commit_and_roll_back() {
    let db = TempDb::new("tx");
    let mut client = db.client();
    client
        .execute_binds(
            "UPDATE emp SET name = :1 WHERE id = :2",
            &["Zed".into(), "1".into()],
        )
        .unwrap();
    // A second connection does not see the uncommitted change.
    let other = rusqlite::Connection::open(&db.0).unwrap();
    let seen: String = other
        .query_row("SELECT name FROM emp WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(seen, "Ana");
    client.rollback().unwrap();
    client.execute("DELETE FROM emp WHERE id = 3").unwrap();
    client.commit().unwrap();
    let left: i64 = other
        .query_row("SELECT count(*) FROM emp", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 2);
    // Commit and rollback with nothing pending are no-ops.
    client.commit().unwrap();
    client.rollback().unwrap();
}

#[test]
fn grid_edits_go_by_rowid() {
    let db = TempDb::new("grid");
    let mut client = db.client();
    let page = client
        .query_table(
            "main",
            "emp",
            &TableFilter::default(),
            TablePage::default().with_rowid(true),
        )
        .unwrap();
    assert_eq!(page.columns[0].name, ora_core::ROWID_COLUMN);
    let rowid = page.rows[1][0].display();
    client
        .update_cell("main", "emp", "name", &rowid, Some("Bud"))
        .unwrap();
    client
        .insert_grid_row("main", "emp", &[("name".into(), "Dedi".into())])
        .unwrap();
    client.delete_grid_row("main", "emp", "1").unwrap();
    let names = client
        .query("SELECT name FROM emp ORDER BY id", FetchLimits::rows(10))
        .unwrap();
    let names: Vec<String> = names.rows.iter().map(|r| r[0].display()).collect();
    assert_eq!(names, ["Bud", "Cici", "Dedi"]);
    // WITHOUT ROWID tables come back without the row id column.
    let kv = client
        .query_table(
            "main",
            "kv",
            &TableFilter::default(),
            TablePage::default().with_rowid(true),
        )
        .unwrap();
    assert_eq!(kv.columns[0].name, "k");
}

#[test]
fn paging_and_filters_use_limit_offset_and_binds() {
    let db = TempDb::new("page");
    let mut client = db.client();
    let filter = TableFilter::new(
        vec![ora_core::FilterCondition::new(
            "name",
            ora_core::FilterOp::Contains,
            "i",
        )],
        Default::default(),
    );
    let page = client
        .query_table("main", "emp", &filter, TablePage::new(1, 1))
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0][1], Value::Text("Cici".into()));
}

#[test]
fn catalog_lists_objects_columns_indexes_and_ddl() {
    let db = TempDb::new("catalog");
    let mut client = db.client();
    assert_eq!(client.list_schemas().unwrap(), ["main"]);
    let objects = client.list_objects("main").unwrap();
    let kinds: Vec<(String, String)> = objects
        .iter()
        .map(|o| (o.name.clone(), o.kind.group_label().to_string()))
        .collect();
    assert!(kinds.contains(&("emp".into(), "Tables".into())));
    assert!(kinds.contains(&("rich".into(), "Views".into())));
    assert!(kinds.contains(&("emp_name".into(), "Indexes".into())));
    assert!(!objects.iter().any(|o| o.name.starts_with("sqlite_")));

    let cols = client.describe_table("main", "emp").unwrap();
    assert_eq!(cols.len(), 3);
    assert!(!cols[1].nullable);
    assert_eq!(cols[2].default.as_deref(), Some("0"));

    let indexes = client.list_indexes("main", "emp").unwrap();
    assert_eq!(indexes[0].name, "emp_name");
    assert_eq!(indexes[0].columns, ["name"]);

    let ddl = client
        .object_ddl("main", "emp", &ora_core::ObjectKind::Table)
        .unwrap();
    assert!(ddl.starts_with("CREATE TABLE emp"), "{ddl}");
    assert!(ddl.contains("CREATE INDEX emp_name"), "{ddl}");
}

#[test]
fn explain_query_plan_builds_a_tree() {
    let db = TempDb::new("plan");
    let mut client = db.client();
    let nodes = client
        .explain_plan_nodes("SELECT * FROM emp WHERE name = 'Ana'")
        .unwrap();
    assert!(!nodes.is_empty());
    assert!(
        nodes.iter().any(|n| n.label().contains("emp_name")),
        "{nodes:?}"
    );
    assert!(!client.explain_plan("SELECT * FROM emp").unwrap().is_empty());
}

#[test]
fn read_only_profile_refuses_writes() {
    let db = TempDb::new("ro");
    let mut client = SqliteClient::new();
    client.connect(&db.profile(true), "").unwrap();
    assert!(client
        .query("SELECT * FROM emp", FetchLimits::rows(10))
        .is_ok());
    let err = client.execute("DELETE FROM emp").unwrap_err();
    assert!(err.to_string().contains("readonly"), "{err}");
}

/// A table of large text and BLOB cells, like a documents table.
fn big_cells(db: &TempDb) {
    let conn = rusqlite::Connection::open(&db.0).unwrap();
    conn.execute_batch(
        "CREATE TABLE doc (id INTEGER PRIMARY KEY, body TEXT, data BLOB);
         INSERT INTO doc (body, data) VALUES (printf('%.*c', 200000, 'x'), zeroblob(300000));
         INSERT INTO doc (body, data) VALUES ('short', x'01');",
    )
    .unwrap();
}

#[test]
fn large_cells_keep_only_a_preview() {
    let db = TempDb::new("big");
    big_cells(&db);
    let mut client = db.client();
    let result = client
        .query(
            "SELECT body, data FROM doc ORDER BY id",
            FetchLimits::grid(),
        )
        .unwrap();
    assert_eq!(result.clipped_cells(), 2);
    let Value::Clipped(body) = &result.rows[0][0] else {
        panic!("{:?}", result.rows[0][0].display())
    };
    assert_eq!(body.full_len, 200_000);
    let Value::Clipped(data) = &result.rows[0][1] else {
        panic!()
    };
    assert_eq!(data.full_len, 300_000);
    // Two 64 KiB previews, not 500 KB of cells.
    assert!(result.heap_bytes() < 2 * ora_core::fetch::MAX_CELL_BYTES + 4096);
    assert_eq!(result.rows[1][0], Value::Text("short".into()));
    assert_eq!(result.rows[1][1], Value::Bytes(vec![1]));
}

#[test]
fn result_byte_limit_stops_early() {
    let db = TempDb::new("bytecap");
    big_cells(&db);
    let mut client = db.client();
    let limits = FetchLimits {
        max_result_bytes: 1024,
        ..FetchLimits::grid()
    };
    let result = client
        .query("SELECT body FROM doc ORDER BY id", limits)
        .unwrap();
    // Even the first 64 KiB preview does not fit in 1 KiB: the row is
    // not stored at all, instead of going over the budget.
    assert_eq!(result.row_count(), 0);
    assert!(result.truncated && result.byte_limited);
    assert!(result.heap_bytes() <= 1024);
}

#[test]
fn full_value_loads_on_demand_by_rowid() {
    let db = TempDb::new("fullcell");
    big_cells(&db);
    let mut client = db.client();
    let full = client
        .fetch_cell("main", "doc", "body", "1", usize::MAX)
        .unwrap();
    assert_eq!(full, Value::Text("x".repeat(200_000)));
    assert!(client
        .fetch_cell("main", "doc", "body", "99", usize::MAX)
        .is_err());
}

#[test]
fn full_value_larger_than_the_budget_is_refused() {
    let db = TempDb::new("fullbudget");
    big_cells(&db);
    let mut client = db.client();
    let err = client
        .fetch_cell("main", "doc", "body", "1", 100_000)
        .unwrap_err()
        .to_string();
    assert!(err.contains("larger than"), "{err}");
    // Exactly enough room still loads (value + enum overhead).
    assert!(client
        .fetch_cell("main", "doc", "body", "1", 300_000)
        .is_ok());
}

/// Advisory SQLite lama diperbaiki di 3.54.0; pastikan build memakai versi itu.
#[test]
fn bundled_sqlite_is_patched() {
    let parts: Vec<u32> = rusqlite::version()
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    assert!(
        (parts[0], parts[1]) >= (3, 54),
        "SQLite terlalu lama: {}",
        rusqlite::version()
    );
}

#[test]
fn table_operations_create_copy_rename_empty_and_drop() {
    use ora_core::ddl::{self, CopyMode, NewColumn, NewTable, ObjectOp, TableKind};
    let db = TempDb::new("ddl");
    let mut client = db.client();
    let d = client.dialect();
    let table = NewTable {
        owner: "main".into(),
        name: "people".into(),
        kind: TableKind::Normal,
        columns: vec![
            NewColumn {
                name: "id".into(),
                data_type: "INTEGER".into(),
                not_null: false,
                primary_key: true,
            },
            NewColumn {
                name: "name".into(),
                data_type: "TEXT".into(),
                not_null: true,
                primary_key: false,
            },
        ],
        ..NewTable::default()
    };
    let run = |client: &mut SqliteClient, sql: String| {
        ddl::run(
            client,
            &ObjectOp::Statements {
                sql: vec![sql],
                summary: "ok".into(),
            },
        )
        .unwrap()
    };
    run(&mut client, ddl::create_table_sql(d, &table).unwrap());
    client
        .execute("INSERT INTO \"people\" (id, name) VALUES (1, 'a'), (2, 'b')")
        .unwrap();
    run(
        &mut client,
        ddl::duplicate_table_sql(d, "main", "people", "people2", CopyMode::StructureAndData)
            .unwrap(),
    );
    run(
        &mut client,
        ddl::duplicate_table_sql(d, "main", "people", "shape", CopyMode::StructureOnly).unwrap(),
    );
    let count = |client: &mut SqliteClient, t: &str| {
        client
            .query(
                &format!("SELECT COUNT(*) FROM \"{t}\""),
                FetchLimits::rows(1),
            )
            .unwrap()
            .rows[0][0]
            .display()
    };
    assert_eq!(count(&mut client, "people2"), "2");
    assert_eq!(count(&mut client, "shape"), "0");
    run(
        &mut client,
        ddl::rename_table_sql(d, "main", "people2", "renamed").unwrap(),
    );
    assert_eq!(count(&mut client, "renamed"), "2");
    run(&mut client, ddl::empty_table_sql(d, "main", "renamed"));
    assert_eq!(count(&mut client, "renamed"), "0");
    run(&mut client, ddl::drop_table_sql(d, "main", "renamed"));
    assert!(client
        .query("SELECT * FROM \"renamed\"", FetchLimits::rows(1))
        .is_err());
    // A hostile name stays one identifier.
    run(
        &mut client,
        ddl::rename_table_sql(d, "main", "shape", "x\"; DROP TABLE people; --").unwrap(),
    );
    assert_eq!(count(&mut client, "people"), "2");
}
