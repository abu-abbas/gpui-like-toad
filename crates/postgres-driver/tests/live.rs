//! Runs against a real PostgreSQL server. Skipped unless
//! `KILAT_TEST_POSTGRES` names one: `host:port/database;user;password`.
//! CI has no server, so these run on a developer machine.

use ora_core::filter::{TableFilter, TablePage};
use ora_core::{ConnectionProfile, DbClient, DbKind, FetchLimits, ObjectKind, Value};
use postgres_driver::PostgresClient;

fn connect() -> Option<PostgresClient> {
    let spec = std::env::var("KILAT_TEST_POSTGRES").ok()?;
    let mut parts = spec.split(';');
    let target = parts.next()?.to_string();
    let user = parts.next()?.to_string();
    let password = parts.next()?.to_string();
    let mut client = PostgresClient::new();
    client
        .connect(
            &ConnectionProfile {
                kind: DbKind::Postgres,
                username: user,
                connect_string: target,
                ..Default::default()
            },
            &password,
        )
        .expect("connect");
    Some(client)
}

fn text(v: &Value) -> String {
    v.display()
}

#[test]
fn session_catalog_query_edit_and_transactions() {
    let Some(mut db) = connect() else {
        eprintln!("KILAT_TEST_POSTGRES not set: skipped");
        return;
    };
    for sql in [
        "DROP SCHEMA IF EXISTS kt_test CASCADE",
        "CREATE SCHEMA kt_test",
        "CREATE TABLE kt_test.emp (id serial PRIMARY KEY, name varchar(20) NOT NULL, \
         salary numeric(10,2), hired date, ts timestamptz, note text, data bytea, \
         flag boolean DEFAULT true, tags text[])",
        "CREATE UNIQUE INDEX emp_name_ix ON kt_test.emp (name)",
        "CREATE INDEX emp_lower_ix ON kt_test.emp (lower(name))",
        "CREATE VIEW kt_test.v_emp AS SELECT id, name FROM kt_test.emp",
        "CREATE TABLE kt_test.dept (id int PRIMARY KEY, boss int REFERENCES kt_test.emp(id) ON DELETE SET NULL, \
         CONSTRAINT pos CHECK (id > 0))",
        "INSERT INTO kt_test.emp (name, salary, hired, ts, note, data, tags) VALUES \
         ('Ana', 1234.50, '2024-01-31', '2024-01-31 10:00:00+00', 'it''s', '\\xdeadbeef', '{a,b}'), \
         ('Bob', NULL, NULL, NULL, NULL, NULL, NULL)",
    ] {
        db.execute(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    db.commit().unwrap();

    // Catalog.
    let schemas = db.list_schemas().unwrap();
    assert_eq!(schemas[0], "public");
    assert!(schemas.contains(&"kt_test".to_string()));
    assert!(!schemas.iter().any(|s| s.starts_with("pg_")));
    let objects = db.list_objects("kt_test").unwrap();
    let find = |n: &str| objects.iter().find(|o| o.name == n).map(|o| o.kind.clone());
    assert_eq!(find("emp"), Some(ObjectKind::Table));
    assert_eq!(find("v_emp"), Some(ObjectKind::View));
    assert_eq!(find("emp_name_ix"), Some(ObjectKind::Index));
    assert_eq!(find("emp_id_seq"), Some(ObjectKind::Sequence));
    assert_eq!(find("emp_pkey"), None, "constraint indexes are not listed");
    let cols = db.describe_table("kt_test", "emp").unwrap();
    assert_eq!(cols[0].name, "id");
    assert!(!cols[0].nullable);
    assert!(cols[0].default.as_deref().unwrap().starts_with("nextval("));
    assert_eq!(cols[1].data_type, "character varying(20)");
    assert_eq!(cols[2].data_type, "numeric(10,2)");
    let idx = db.list_indexes("kt_test", "emp").unwrap();
    assert!(idx
        .iter()
        .any(|i| i.name == "emp_name_ix" && i.unique && i.columns == ["name"]));
    assert!(idx
        .iter()
        .any(|i| i.name == "emp_lower_ix" && i.index_type.starts_with("FUNCTION")));
    let cons = db.list_constraints("kt_test", "dept").unwrap();
    assert!(cons
        .iter()
        .any(|c| c.columns == ["boss"] && c.ref_table == "emp" && c.on_delete == "SET NULL"));
    assert!(cons
        .iter()
        .any(|c| c.name == "pos" && c.condition == "((id > 0))"));
    let ddl = db.object_ddl("kt_test", "emp", &ObjectKind::Table).unwrap();
    assert!(ddl.contains("CREATE TABLE \"kt_test\".\"emp\""), "{ddl}");
    assert!(ddl.contains("CREATE UNIQUE INDEX emp_name_ix"), "{ddl}");
    assert!(db
        .object_ddl("kt_test", "v_emp", &ObjectKind::View)
        .unwrap()
        .contains("CREATE OR REPLACE VIEW"));
    let catalog = db.catalog_objects("kt_test").unwrap();
    assert_eq!(
        catalog
            .iter()
            .find(|o| o.name == "emp")
            .unwrap()
            .columns
            .len(),
        9
    );

    // Types arrive as the server writes them.
    let r = db
        .query(
            "SELECT * FROM kt_test.emp ORDER BY id",
            FetchLimits::rows(100),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let ana = &r.rows[0];
    assert_eq!(ana[0], Value::Integer(1));
    assert_eq!(text(&ana[2]), "1234.50");
    assert_eq!(text(&ana[3]), "2024-01-31");
    assert_eq!(ana[5], Value::Text("it's".into()));
    assert_eq!(ana[6], Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(text(&ana[7]), "true");
    assert_eq!(text(&ana[8]), "{a,b}");
    assert_eq!(r.rows[1][2], Value::Null);
    assert_eq!(r.columns[2].data_type, "NUMERIC");

    // Limits: one row past the limit marks the result truncated.
    let r = db
        .query(
            "SELECT g FROM generate_series(1, 2000) g",
            FetchLimits::rows(1200),
        )
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (1200, true));
    let r = db
        .query(
            "SELECT g FROM generate_series(1, 5) g",
            FetchLimits::rows(5),
        )
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (5, false));

    // Data tab page with row id, filter-free; then edit by that id.
    let page = db
        .query_table(
            "kt_test",
            "emp",
            &TableFilter::default(),
            TablePage::new(0, 10).with_rowid(true),
        )
        .unwrap();
    assert_eq!(page.columns[0].name, "__ROWID__");
    // A table with a primary key is edited by it: the id survives updates.
    let rowid = text(&page.rows[0][0]);
    assert_eq!(rowid, "[1]");
    db.update_cell("kt_test", "emp", "salary", &rowid, Some("99.5"))
        .unwrap();
    db.update_cell("kt_test", "emp", "note", &rowid, Some("edited twice"))
        .unwrap();
    db.update_cell("kt_test", "emp", "note", &rowid, None)
        .unwrap();
    let err = db
        .update_cell("kt_test", "emp", "note", "[999]", Some("x"))
        .unwrap_err();
    assert!(err.0.contains("no longer exists"), "{err}");
    // The failed statement did not abort the transaction.
    let r = db
        .query_binds(
            "SELECT salary FROM kt_test.emp WHERE id = :1",
            &["1".to_string()],
            FetchLimits::rows(5),
        )
        .unwrap();
    assert_eq!(text(&r.rows[0][0]), "99.50");
    let cell = db
        .fetch_cell("kt_test", "emp", "name", &rowid, 1000)
        .unwrap();
    assert_eq!(text(&cell), "Ana");
    db.delete_grid_row("kt_test", "emp", "[2]").unwrap();
    let r = db
        .query("SELECT count(*) FROM kt_test.emp", FetchLimits::rows(5))
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Integer(1));
    db.rollback().unwrap();
    let r = db
        .query(
            "SELECT salary FROM kt_test.emp WHERE id = 1",
            FetchLimits::rows(5),
        )
        .unwrap();
    assert_eq!(text(&r.rows[0][0]), "1234.50", "rollback undid the edit");

    // A table without a key falls back to ctid, which an update changes.
    db.execute("CREATE TABLE kt_test.loose (a text, b text)")
        .unwrap();
    db.execute("INSERT INTO kt_test.loose VALUES ('x', 'y')")
        .unwrap();
    db.commit().unwrap();
    let page = db
        .query_table(
            "kt_test",
            "loose",
            &TableFilter::default(),
            TablePage::new(0, 10).with_rowid(true),
        )
        .unwrap();
    let ctid = text(&page.rows[0][0]);
    assert!(ctid.starts_with('('), "{ctid}");
    db.update_cell("kt_test", "loose", "b", &ctid, Some("z"))
        .unwrap();
    let err = db
        .update_cell("kt_test", "loose", "b", &ctid, Some("w"))
        .unwrap_err();
    assert!(err.0.contains("no longer exists"), "{err}");
    db.rollback().unwrap();

    // Identity columns count as generated, and the table script keeps them.
    db.execute(
        "CREATE TABLE kt_test.ident (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, v text)",
    )
    .unwrap();
    db.commit().unwrap();
    let cols = db.describe_table("kt_test", "ident").unwrap();
    assert!(db.dialect().is_auto_generated(&cols[0]));
    let ddl = db
        .object_ddl("kt_test", "ident", &ObjectKind::Table)
        .unwrap();
    assert!(ddl.contains("GENERATED ALWAYS AS IDENTITY"), "{ddl}");
    assert!(!ddl.contains("DEFAULT GENERATED"), "{ddl}");

    // DDL is transactional: a rolled back CREATE leaves nothing.
    db.execute("CREATE TABLE kt_test.temp_t (a int)").unwrap();
    db.rollback().unwrap();
    assert!(db
        .query("SELECT * FROM kt_test.temp_t", FetchLimits::rows(1))
        .is_err());
    assert_eq!(
        ora_core::ddl::drop_object_sql(
            db.dialect(),
            "kt_test",
            "g",
            &ObjectKind::Trigger,
            Some("emp")
        )
        .unwrap(),
        "DROP TRIGGER \"g\" ON \"kt_test\".\"emp\""
    );

    // Errors are readable and leave the session usable.
    let err = db
        .query("SELECT * FROM kt_test.nope", FetchLimits::rows(5))
        .unwrap_err();
    assert!(err.0.contains("does not exist"), "{err}");
    let err = db
        .execute("INSERT INTO kt_test.emp (name) VALUES ('Ana')")
        .unwrap_err();
    assert!(err.0.contains("duplicate key"), "{err}");
    db.execute("INSERT INTO kt_test.emp (name) VALUES ('Cy')")
        .unwrap();
    db.commit().unwrap();
    assert!(db.explain_plan("SELECT 1").unwrap()[0].contains("Result"));
    let r = db
        .query("SHOW server_version", FetchLimits::rows(5))
        .unwrap();
    assert_eq!(r.rows.len(), 1);

    db.execute("DROP SCHEMA kt_test CASCADE").unwrap();
    db.commit().unwrap();
    db.disconnect();
    assert!(!db.is_connected());
}

#[test]
fn bad_logins_and_targets_are_readable() {
    let Some(spec) = std::env::var("KILAT_TEST_POSTGRES").ok() else {
        return;
    };
    let target = spec.split(';').next().unwrap().to_string();
    let mut db = PostgresClient::new();
    let profile = |target: &str| ConnectionProfile {
        kind: DbKind::Postgres,
        username: "postgres".into(),
        connect_string: target.into(),
        ..Default::default()
    };
    let err = db.connect(&profile(&target), "wrong-password").unwrap_err();
    assert!(err.0.starts_with("PostgreSQL:"), "{err}");
    assert!(db.connect(&profile(""), "x").is_err());
    assert!(db.connect(&profile("127.0.0.1:notaport/db"), "x").is_err());
}

#[test]
fn explain_analyze_runs_and_always_rolls_back() {
    let Some(mut db) = connect() else {
        return;
    };
    for sql in [
        "DROP TABLE IF EXISTS kt_ea",
        "CREATE TABLE kt_ea (id int PRIMARY KEY, v text)",
        "INSERT INTO kt_ea VALUES (1, 'a'), (2, 'b')",
    ] {
        db.execute(sql).unwrap();
    }
    db.commit().unwrap();
    let count = |db: &mut PostgresClient| {
        db.query("SELECT count(*) FROM kt_ea", FetchLimits::rows(5))
            .unwrap()
            .rows[0][0]
            .display()
    };

    // A read shows measured times.
    let plan = db.explain_analyze("SELECT * FROM kt_ea;").unwrap();
    assert!(plan.iter().any(|l| l.contains("actual time")), "{plan:?}");

    // A DELETE executes (the plan reports its rows) and is undone.
    let plan = db.explain_analyze("DELETE FROM kt_ea").unwrap();
    assert!(plan.iter().any(|l| l.contains("Delete")), "{plan:?}");
    assert_eq!(count(&mut db), "2");

    // Inside the caller's transaction: its own changes survive.
    db.execute("INSERT INTO kt_ea VALUES (3, 'c')").unwrap();
    db.explain_analyze("DELETE FROM kt_ea").unwrap();
    assert_eq!(count(&mut db), "3");
    db.rollback().unwrap();
    assert_eq!(count(&mut db), "2");

    // Refused: scripts, DDL, other commands. Nothing ran.
    for sql in [
        "SELECT 1; DELETE FROM kt_ea",
        "DROP TABLE kt_ea",
        "TRUNCATE kt_ea",
        "",
    ] {
        assert!(db.explain_analyze(sql).is_err(), "{sql}");
    }
    assert_eq!(count(&mut db), "2");

    // A failing statement leaves the session usable, in and out of a tx.
    assert!(db.explain_analyze("SELECT * FROM missing_table").is_err());
    db.execute("INSERT INTO kt_ea VALUES (4, 'd')").unwrap();
    assert!(db.explain_analyze("SELECT * FROM missing_table").is_err());
    assert_eq!(count(&mut db), "3");
    db.rollback().unwrap();

    db.execute("DROP TABLE kt_ea").unwrap();
    db.commit().unwrap();
}
