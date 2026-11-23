//! End to end: the real extension program, driven through `RemoteClient`.

use ora_core::extension_client::RemoteClient;
use ora_core::filter::{FilterCondition, FilterJoin, FilterOp};
use ora_core::{
    ConnectionProfile, DbClient, DbKind, FetchLimits, ObjectKind, TableFilter, TablePage, Value,
};
use std::path::{Path, PathBuf};

fn program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kilat-ext-sqlite"))
}

fn client() -> RemoteClient {
    RemoteClient::new(program(), "sqlite", DbKind::Sqlite, "test")
}

struct TempDb {
    dir: PathBuf,
    file: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let name = format!("kilat-ext-sqlite-{}", ora_core::store::new_profile_id());
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.db");
        let conn = rusqlite::Connection::open(&file).unwrap();
        conn.execute_batch(
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT);
             CREATE INDEX people_name ON people (name);
             INSERT INTO people VALUES (1, 'Ada', 'first'), (2, 'Bob', NULL), (3, 'Cy', 'x');
             CREATE TABLE big (v TEXT);",
        )
        .unwrap();
        Self { dir, file }
    }

    fn profile(&self) -> ConnectionProfile {
        profile_for(&self.file)
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn profile_for(path: &Path) -> ConnectionProfile {
    ConnectionProfile {
        name: "t".into(),
        kind: DbKind::Sqlite,
        connect_string: path.to_string_lossy().into_owned(),
        ..ConnectionProfile::default()
    }
}

fn connected(db: &TempDb) -> RemoteClient {
    let mut c = client();
    c.connect(&db.profile(), "").unwrap();
    c
}

#[test]
fn connect_reports_the_session_and_disconnect_ends_it() {
    let db = TempDb::new();
    let mut c = client();
    assert!(!c.is_connected());
    let info = c.connect(&db.profile(), "").unwrap();
    assert_eq!(info.database, "test.db");
    assert!(info.version.starts_with("SQLite"), "{}", info.version);
    assert!(c.is_connected());
    c.disconnect();
    assert!(!c.is_connected());
    assert!(c.query("SELECT 1", FetchLimits::rows(10)).is_err());
}

#[test]
fn a_database_error_reaches_the_app_and_the_session_survives() {
    let db = TempDb::new();
    let mut c = connected(&db);
    let err = c.query("SELEKT 1", FetchLimits::rows(10)).unwrap_err();
    assert!(err.to_string().contains("SELEKT"), "{err}");
    assert!(c.is_connected());
    assert!(c.query("SELECT 1", FetchLimits::rows(10)).is_ok());
}

#[test]
fn connecting_to_a_missing_file_fails_cleanly() {
    let db = TempDb::new();
    let mut c = client();
    let err = c
        .connect(&profile_for(&db.dir.join("nope.db")), "")
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(!c.is_connected());
}

#[test]
fn an_extension_with_another_id_is_refused() {
    let db = TempDb::new();
    let mut c = RemoteClient::new(program(), "redis", DbKind::Sqlite, "test");
    let err = c.connect(&db.profile(), "").unwrap_err();
    assert!(err.to_string().contains("sqlite"), "{err}");
    assert!(!c.is_connected());
}

#[test]
fn queries_return_typed_cells_and_columns() {
    let db = TempDb::new();
    let mut c = connected(&db);
    let r = c
        .query(
            "SELECT id, name, note FROM people ORDER BY id",
            FetchLimits::rows(10),
        )
        .unwrap();
    assert_eq!(r.columns.len(), 3);
    assert_eq!(r.columns[1].name, "name");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][0], Value::Integer(1));
    assert_eq!(r.rows[0][1], Value::Text("Ada".into()));
    assert_eq!(r.rows[1][2], Value::Null);
    assert!(!r.truncated);
}

#[test]
fn binds_and_the_row_limit_travel() {
    let db = TempDb::new();
    let mut c = connected(&db);
    let r = c
        .query_binds(
            "SELECT name FROM people WHERE id > :1 ORDER BY id",
            &["1".to_string()],
            FetchLimits::rows(10),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let cut = c
        .query("SELECT id FROM people ORDER BY id", FetchLimits::rows(2))
        .unwrap();
    assert_eq!(cut.rows.len(), 2);
    assert!(cut.truncated);
}

#[test]
fn big_results_arrive_in_pieces() {
    let db = TempDb::new();
    let mut c = connected(&db);
    // About 3 MB of text: several chunks.
    let n = c
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
             INSERT INTO big SELECT printf('%01000d', i) FROM n",
        )
        .unwrap();
    assert_eq!(n, 3000);
    let limits = FetchLimits {
        max_rows: 5000,
        max_cell_bytes: 64 * 1024,
        max_result_bytes: 64 * 1024 * 1024,
        read_lobs: false,
    };
    let r = c.query("SELECT v FROM big", limits).unwrap();
    assert_eq!(r.rows.len(), 3000);
    assert!(matches!(&r.rows[2999][0], Value::Text(t) if t.len() == 1000));
}

#[test]
fn clipped_cells_keep_their_length() {
    let db = TempDb::new();
    let mut c = connected(&db);
    c.execute("INSERT INTO big VALUES (printf('%0200d', 7))")
        .unwrap();
    let limits = FetchLimits {
        max_cell_bytes: 50,
        ..FetchLimits::rows(10)
    };
    let r = c.query("SELECT v FROM big", limits).unwrap();
    match &r.rows[0][0] {
        Value::Clipped(c) => assert_eq!(c.full_len, 200),
        other => panic!("{other:?}"),
    }
}

#[test]
fn changes_wait_for_commit_and_rollback_undoes_them() {
    let db = TempDb::new();
    let mut c = connected(&db);
    assert_eq!(
        c.execute("UPDATE people SET note = 'changed' WHERE id = 1")
            .unwrap(),
        1
    );
    c.rollback().unwrap();
    let r = c
        .query("SELECT note FROM people WHERE id = 1", FetchLimits::rows(5))
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Text("first".into()));
    c.execute_binds(
        "UPDATE people SET note = :1 WHERE id = :2",
        &["kept".to_string(), "1".to_string()],
    )
    .unwrap();
    c.commit().unwrap();
    c.disconnect();
    let mut again = connected(&db);
    let r = again
        .query("SELECT note FROM people WHERE id = 1", FetchLimits::rows(5))
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Text("kept".into()));
}

#[test]
fn the_catalog_calls_work() {
    let db = TempDb::new();
    let mut c = connected(&db);
    let schemas = c.list_schemas().unwrap();
    assert!(
        schemas.iter().any(|s| s.eq_ignore_ascii_case("main")),
        "{schemas:?}"
    );
    let objects = c.list_objects("main").unwrap();
    assert!(objects
        .iter()
        .any(|o| o.name == "people" && o.kind == ObjectKind::Table));
    let columns = c.describe_table("main", "people").unwrap();
    assert_eq!(columns.len(), 3);
    assert!(!columns[1].nullable);
    let indexes = c.list_indexes("main", "people").unwrap();
    assert!(indexes.iter().any(|i| i.name == "people_name"));
    assert!(c.list_constraints("main", "people").is_ok());
    assert!(c.trigger_scripts("main", "people").unwrap().is_empty());
    let ddl = c
        .object_ddl_with("main", "people", &ObjectKind::Table, false)
        .unwrap();
    assert!(ddl.to_uppercase().contains("CREATE TABLE"), "{ddl}");
    let plan = c
        .explain_plan_nodes("SELECT * FROM people WHERE id = 1")
        .unwrap();
    assert!(!plan.is_empty());
    assert!(!c.explain_plan("SELECT * FROM people").unwrap().is_empty());
}

#[test]
fn the_data_tab_fetch_filters_and_pages() {
    let db = TempDb::new();
    let mut c = connected(&db);
    let filter = TableFilter::new(
        vec![FilterCondition::new("name", FilterOp::Contains, "b")],
        FilterJoin::And,
    );
    let mut page = TablePage::new(0, 10);
    page.rowid = true;
    let r = c.query_table("main", "people", &filter, page).unwrap();
    assert_eq!(r.rows.len(), 1);
    let name_col = r.columns.iter().position(|c| c.name == "name").unwrap();
    assert_eq!(r.rows[0][name_col], Value::Text("Bob".into()));
}

#[test]
fn a_stuck_query_times_out_and_the_client_reconnects() {
    let db = TempDb::new();
    let mut c = client().with_call_timeout(std::time::Duration::from_millis(500));
    c.connect(&db.profile(), "").unwrap();
    let err = c
        .query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
             SELECT count(*) FROM n",
            FetchLimits::rows(1),
        )
        .unwrap_err();
    assert!(err.to_string().contains("did not answer"), "{err}");
    assert!(!c.is_connected());
    assert!(c.query("SELECT 1", FetchLimits::rows(1)).is_err());
    assert!(c.connect(&db.profile(), "").is_ok());
    assert!(c.query("SELECT 1", FetchLimits::rows(1)).is_ok());
}
