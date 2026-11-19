use std::path::PathBuf;

use ora_core::{
    ConnectionAccess, ConnectionProfile, DbClient, DbKind, TableFilter, TablePage, Value,
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
    assert!(err.to_string().contains("tidak ditemukan"), "{err}");
    assert!(!path.exists());
}

#[test]
fn query_maps_sqlite_types_and_caps_rows() {
    let db = TempDb::new("types");
    let mut client = db.client();
    let result = client
        .query("SELECT id, name, salary FROM emp ORDER BY id;", 2)
        .unwrap();
    assert_eq!(result.columns[0].data_type, "INTEGER");
    assert_eq!(result.rows.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.rows[0][0], Value::Integer(1));
    assert_eq!(result.rows[1][2], Value::Float(150.5));
    let kv = client.query("SELECT v FROM kv", 10).unwrap();
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
        .query("SELECT name FROM emp ORDER BY id", 10)
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
    assert!(client.query("SELECT * FROM emp", 10).is_ok());
    let err = client.execute("DELETE FROM emp").unwrap_err();
    assert!(err.to_string().contains("readonly"), "{err}");
}
