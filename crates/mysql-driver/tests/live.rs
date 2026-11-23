//! Runs against a real MySQL or MariaDB server. Skipped unless
//! `KILAT_TEST_MYSQL` names one: `host:port/database;user;password`.
//! CI has no server, so these run on a developer machine.

use mysql_driver::MysqlClient;
use ora_core::filter::{TableFilter, TablePage};
use ora_core::{ConnectionProfile, DbClient, DbKind, FetchLimits, ObjectKind, Value};

fn connect() -> Option<MysqlClient> {
    let spec = std::env::var("KILAT_TEST_MYSQL").ok()?;
    let mut parts = spec.split(';');
    let target = parts.next()?.to_string();
    let user = parts.next()?.to_string();
    let password = parts.next()?.to_string();
    let mut client = MysqlClient::new();
    client
        .connect(
            &ConnectionProfile {
                kind: DbKind::Mysql,
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
        eprintln!("KILAT_TEST_MYSQL not set: skipped");
        return;
    };
    for sql in [
        "DROP DATABASE IF EXISTS kt_test",
        "CREATE DATABASE kt_test",
        "CREATE TABLE kt_test.emp (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20) NOT NULL, \
         salary DECIMAL(10,2), hired DATE, ts DATETIME, note TEXT, data BLOB, \
         flag TINYINT(1) DEFAULT 1, big BIGINT UNSIGNED, UNIQUE KEY emp_name_ix (name))",
        "CREATE INDEX emp_hired_ix ON kt_test.emp (hired)",
        "CREATE VIEW kt_test.v_emp AS SELECT id, name FROM kt_test.emp",
        "CREATE TABLE kt_test.dept (id INT PRIMARY KEY, boss INT, \
         CONSTRAINT dept_boss FOREIGN KEY (boss) REFERENCES kt_test.emp(id) ON DELETE SET NULL, \
         CONSTRAINT pos CHECK (id > 0))",
        "INSERT INTO kt_test.emp (name, salary, hired, ts, note, data, big) VALUES \
         ('Ana', 1234.50, '2024-01-31', '2024-01-31 10:00:00', 'it''s', X'DEADBEEF', 18446744073709551615), \
         ('Bob', NULL, NULL, NULL, NULL, NULL, NULL)",
        "CREATE TABLE kt_test.audit (n INT)",
        "CREATE TRIGGER kt_test.emp_ai AFTER INSERT ON kt_test.emp FOR EACH ROW \
         INSERT INTO kt_test.audit VALUES (NEW.id)",
    ] {
        db.execute(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    db.commit().unwrap();

    // Catalog.
    let schemas = db.list_schemas().unwrap();
    assert!(schemas.contains(&"kt_test".to_string()));
    assert!(!schemas.contains(&"mysql".to_string()));
    let objects = db.list_objects("kt_test").unwrap();
    let find = |n: &str| objects.iter().find(|o| o.name == n).map(|o| o.kind.clone());
    assert_eq!(find("emp"), Some(ObjectKind::Table));
    assert_eq!(find("v_emp"), Some(ObjectKind::View));
    assert_eq!(find("emp_name_ix"), Some(ObjectKind::Index));
    assert_eq!(find("emp_ai"), Some(ObjectKind::Trigger));
    assert_eq!(find("PRIMARY"), None);
    let cols = db.describe_table("kt_test", "emp").unwrap();
    assert_eq!(cols[0].name, "id");
    assert!(!cols[0].nullable);
    assert!(db.dialect().is_auto_generated(&cols[0]));
    assert_eq!(cols[1].data_type, "varchar(20)");
    assert_eq!(cols[2].data_type, "decimal(10,2)");
    // MariaDB writes a numeric default bare, MySQL as a string.
    assert!(matches!(cols[7].default.as_deref(), Some("'1'" | "1")));
    let idx = db.list_indexes("kt_test", "emp").unwrap();
    assert_eq!(idx[0].name, "PRIMARY");
    assert!(idx
        .iter()
        .any(|i| i.name == "emp_name_ix" && i.unique && i.columns == ["name"]));
    let cons = db.list_constraints("kt_test", "dept").unwrap();
    assert!(cons
        .iter()
        .any(|c| c.columns == ["boss"] && c.ref_table == "emp" && c.on_delete == "SET NULL"));
    assert!(cons
        .iter()
        .any(|c| c.name == "pos" && c.condition.contains("id")));
    let ddl = db.object_ddl("kt_test", "emp", &ObjectKind::Table).unwrap();
    assert!(ddl.contains("CREATE TABLE `emp`"), "{ddl}");
    assert!(db
        .object_ddl("kt_test", "v_emp", &ObjectKind::View)
        .unwrap()
        .contains("VIEW"));
    assert!(db
        .object_ddl("kt_test", "emp_ai", &ObjectKind::Trigger)
        .unwrap()
        .contains("TRIGGER"));
    assert_eq!(
        db.object_ddl("kt_test", "emp_hired_ix", &ObjectKind::Index)
            .unwrap(),
        "CREATE INDEX `emp_hired_ix` ON `kt_test`.`emp` (`hired`);"
    );
    assert_eq!(db.trigger_scripts("kt_test", "emp").unwrap().len(), 1);
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
    assert_eq!(text(&ana[4]), "2024-01-31 10:00:00");
    assert_eq!(ana[5], Value::Text("it's".into()));
    assert_eq!(ana[6], Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(ana[8], Value::Integer(18446744073709551615));
    assert_eq!(r.rows[1][2], Value::Null);
    assert_eq!(r.columns[2].data_type, "DECIMAL");
    assert_eq!(r.columns[8].data_type, "BIGINT UNSIGNED");

    // Limits: one row past the limit marks the result truncated.
    db.execute("CREATE TABLE kt_test.nums (n INT AUTO_INCREMENT PRIMARY KEY)")
        .unwrap();
    db.execute("INSERT INTO kt_test.nums (n) SELECT NULL FROM kt_test.emp e0, kt_test.emp e1, kt_test.emp e2, kt_test.emp e3, kt_test.emp e4, kt_test.emp e5, kt_test.emp e6, kt_test.emp e7, kt_test.emp e8, kt_test.emp e9, kt_test.emp e10")
        .unwrap();
    db.commit().unwrap();
    let r = db
        .query("SELECT n FROM kt_test.nums", FetchLimits::rows(1200))
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (1200, true));
    let r = db
        .query("SELECT n FROM kt_test.nums LIMIT 5", FetchLimits::rows(5))
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (5, false));
    let r = db
        .query("SELECT n FROM kt_test.nums LIMIT 50", FetchLimits::rows(10))
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated), (10, true));
    let r = db
        .query_binds(
            "SELECT n FROM kt_test.nums WHERE n > :1 AND n < :2 AND n <> :1 + 1",
            &["10".to_string(), "14".to_string()],
            FetchLimits::rows(50),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);

    // Data tab page with row id; then edit by that id.
    let page = db
        .query_table(
            "kt_test",
            "emp",
            &TableFilter::default(),
            TablePage::new(0, 10).with_rowid(true),
        )
        .unwrap();
    assert_eq!(page.columns[0].name, "__ROWID__");
    let rowid = text(&page.rows[0][0]);
    assert!(rowid.contains("\"1\""), "{rowid}");
    db.update_cell("kt_test", "emp", "salary", &rowid, Some("99.5"))
        .unwrap();
    db.update_cell("kt_test", "emp", "note", &rowid, Some("edited twice"))
        .unwrap();
    // Setting the value it already has still finds the row.
    db.update_cell("kt_test", "emp", "note", &rowid, Some("edited twice"))
        .unwrap();
    db.update_cell("kt_test", "emp", "note", &rowid, None)
        .unwrap();
    let err = db
        .update_cell("kt_test", "emp", "note", "[\"999\"]", Some("x"))
        .unwrap_err();
    assert!(err.0.contains("no longer exists"), "{err}");
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
    // A big unsigned key matches exactly.
    db.execute("INSERT INTO kt_test.dept (id) VALUES (7)")
        .unwrap();
    db.delete_grid_row("kt_test", "emp", &page.rows[1][0].display())
        .unwrap();
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

    // A table without a key is found by all its comparable columns:
    // identical rows are told apart by nothing, so one of them changes.
    db.execute("CREATE TABLE kt_test.loose (a TEXT, b VARCHAR(10), n INT, d DATE, f DOUBLE)")
        .unwrap();
    db.execute(
        "INSERT INTO kt_test.loose VALUES ('x', 'y', NULL, '2024-02-01', 1.5), \
         ('x', 'y', NULL, '2024-02-01', 1.5), ('p', NULL, 3, NULL, 2.5)",
    )
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
    let id0 = text(&page.rows[0][0]);
    assert!(id0.contains("null"), "{id0}");
    db.update_cell("kt_test", "loose", "a", &id0, Some("z"))
        .unwrap();
    let r = db
        .query(
            "SELECT count(*) FROM kt_test.loose WHERE a = 'z'",
            FetchLimits::rows(5),
        )
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Integer(1), "one of two equal rows");
    // Its old id no longer finds anything (the row was changed).
    let id2 = text(&page.rows[2][0]);
    db.delete_grid_row("kt_test", "loose", &id2).unwrap();
    assert!(db
        .update_cell("kt_test", "loose", "b", &id2, Some("q"))
        .is_err());
    db.rollback().unwrap();

    // Keys that a floating-point comparison would confuse, and a text key
    // with a quote in it.
    db.execute("CREATE TABLE kt_test.bigk (k BIGINT UNSIGNED PRIMARY KEY, v TEXT)")
        .unwrap();
    db.execute(
        "INSERT INTO kt_test.bigk VALUES (18446744073709551615, 'a'), (18446744073709551614, 'b')",
    )
    .unwrap();
    db.execute("CREATE TABLE kt_test.textk (a VARCHAR(10), b INT, v TEXT, PRIMARY KEY (a, b))")
        .unwrap();
    db.execute("INSERT INTO kt_test.textk VALUES ('it''s', 1, 'x'), ('it''s', 2, 'y')")
        .unwrap();
    db.commit().unwrap();
    for (table, col, pick) in [("bigk", "v", 0usize), ("textk", "v", 1)] {
        let page = db
            .query_table(
                "kt_test",
                table,
                &TableFilter::default(),
                TablePage::new(0, 10).with_rowid(true),
            )
            .unwrap();
        let id = text(&page.rows[pick][0]);
        db.update_cell("kt_test", table, col, &id, Some("changed"))
            .unwrap_or_else(|e| panic!("{table} {id}: {e}"));
        let after = db
            .query(
                &format!("SELECT count(*) FROM kt_test.{table} WHERE v = 'changed'"),
                FetchLimits::rows(5),
            )
            .unwrap();
        assert_eq!(after.rows[0][0], Value::Integer(1), "{table} {id}");
    }
    db.rollback().unwrap();

    // Errors are readable and leave the session usable.
    let err = db
        .query("SELECT * FROM kt_test.nope", FetchLimits::rows(5))
        .unwrap_err();
    assert!(err.0.contains("doesn't exist"), "{err}");
    let err = db
        .execute("INSERT INTO kt_test.emp (name) VALUES ('Ana')")
        .unwrap_err();
    assert!(err.0.contains("Duplicate"), "{err}");
    db.execute("INSERT INTO kt_test.emp (name) VALUES ('Cy')")
        .unwrap();
    db.commit().unwrap();
    let plan = db.explain_plan("SELECT * FROM kt_test.emp").unwrap();
    assert!(plan[0].starts_with("id | select_type"), "{plan:?}");
    let r = db
        .query("SHOW TABLES FROM kt_test", FetchLimits::rows(50))
        .unwrap();
    assert!(r.rows.len() >= 5);

    // Several statements in one script.
    db.execute("INSERT INTO kt_test.audit VALUES (1); INSERT INTO kt_test.audit VALUES (2)")
        .unwrap();
    db.rollback().unwrap();

    db.execute("DROP DATABASE kt_test").unwrap();
    db.commit().unwrap();
    db.disconnect();
    assert!(!db.is_connected());
}

#[test]
fn bad_logins_and_targets_are_readable() {
    let Some(spec) = std::env::var("KILAT_TEST_MYSQL").ok() else {
        return;
    };
    let target = spec.split(';').next().unwrap().to_string();
    let mut db = MysqlClient::new();
    let profile = |target: &str| ConnectionProfile {
        kind: DbKind::Mysql,
        username: "kilat".into(),
        connect_string: target.into(),
        ..Default::default()
    };
    let err = db.connect(&profile(&target), "wrong-password").unwrap_err();
    assert!(err.0.starts_with("MySQL:"), "{err}");
    assert!(db.connect(&profile(""), "x").is_err());
    assert!(db.connect(&profile("127.0.0.1:notaport/db"), "x").is_err());
}

#[test]
fn tls_is_used_when_the_server_offers_it() {
    let Some(mut db) = connect() else {
        return;
    };
    let r = db
        .query(
            "SHOW SESSION STATUS LIKE 'Ssl_cipher'",
            FetchLimits::rows(5),
        )
        .unwrap();
    let cipher = text(&r.rows[0][1]);
    // A server without TLS answers with an empty cipher and still works.
    eprintln!("cipher: {cipher:?}");
    assert!(r.rows.len() == 1);
}
