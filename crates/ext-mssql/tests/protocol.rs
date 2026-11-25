//! End to end: the real extension program, driven through `RemoteClient`.
//! The database part needs a server: set `KILAT_TEST_MSSQL` to
//! `host:port/database;user;password` (CI has none, so it is skipped there).
//! The server needs two databases, `dev` and `staging`, each with a table
//! `dbo.users` of different columns and contents (see `docs/mssql-test-data.sql`
//! in the pull request that added this test).
//! Set `KILAT_TEST_MSSQL_LEGACY` with the same format for a TLS 1.0-only
//! server. That test account must be able to read its encrypted session status.

use std::path::PathBuf;

use ora_core::extension_client::RemoteClient;
use ora_core::filter::{select_sql_in, TableTarget};
use ora_core::{
    ConnectionProfile, DbClient, DbKind, FetchLimits, FilterCondition, FilterOp, ObjectKind,
    TableFilter, TablePage, Value,
};

fn client() -> RemoteClient {
    RemoteClient::new(
        PathBuf::from(env!("CARGO_BIN_EXE_kilat-ext-mssql")),
        "mssql",
        DbKind::Mssql,
        "test",
    )
}

fn profile(target: &str, user: &str) -> ConnectionProfile {
    let mut p = ConnectionProfile {
        name: "t".into(),
        kind: DbKind::Mssql,
        username: user.into(),
        connect_string: target.into(),
        ..ConnectionProfile::default()
    };
    // The test server has a self-signed certificate.
    p.access.trust_server_certificate = true;
    p
}

fn server() -> Option<(String, String, String)> {
    server_from_env("KILAT_TEST_MSSQL")
}

fn server_from_env(variable: &str) -> Option<(String, String, String)> {
    let Some(spec) = std::env::var(variable).ok() else {
        eprintln!("{variable} not set: skipped");
        return None;
    };
    let mut parts = spec.split(';');
    Some((
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
    ))
}

#[test]
fn legacy_tls_requires_opt_in_and_stays_encrypted() {
    let Some((target, user, password)) = server_from_env("KILAT_TEST_MSSQL_LEGACY") else {
        return;
    };
    let mut profile = profile(&target, &user);
    let mut c = client();
    assert!(c.connect(&profile, &password).is_err());

    // The legacy option changes the allowed protocol versions only.
    profile.access.allow_legacy_tls = true;
    profile.access.trust_server_certificate = false;
    assert!(c.connect(&profile, &password).is_err());
    profile.access.trust_server_certificate = true;
    c.connect(&profile, &password).unwrap();
    let result = c
        .query(
            "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID",
            FetchLimits::rows(1),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Text("TRUE".into())]]);

    profile.access.allow_legacy_tls = false;
    assert!(c.connect(&profile, &password).is_err());
}

#[test]
fn the_program_answers_the_handshake_and_reports_errors() {
    let mut c = client();
    // Nothing listens on port 1: the handshake works, the connect fails
    // with the driver's own message.
    let err = c.connect(&profile("127.0.0.1:1/db", "u"), "p").unwrap_err();
    assert!(err.0.contains("SQL Server"), "{err}");
    // And the program is still alive for the next call.
    let err = c.connect(&profile("", "u"), "p").unwrap_err();
    assert!(err.0.contains("host"), "{err}");
}

/// Without trusting the certificate, a server with a self-signed one is
/// refused: TLS is checked by default.
#[test]
fn a_self_signed_certificate_is_refused_unless_trusted() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut strict = profile(&target, &user);
    strict.access.trust_server_certificate = false;
    let mut c = client();
    let err = c.connect(&strict, &password).unwrap_err();
    assert!(
        err.0.to_ascii_lowercase().contains("certificate")
            || err.0.to_ascii_lowercase().contains("tls"),
        "{err}"
    );
}

fn first_column(r: &ora_core::QueryResult) -> Vec<String> {
    r.rows.iter().map(|row| row[0].display()).collect()
}

/// The acceptance test: two tabs, `dev.dbo.users` and `staging.dbo.users`,
/// same names and different columns, stay apart for page 2, count,
/// describe, indexes, script and the SQL the editor would be given.
#[test]
fn two_databases_with_the_same_table_name_stay_apart() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut c = client();
    let info = c.connect(&profile(&target, &user), &password).unwrap();
    assert!(info.version.contains("SQL Server"), "{}", info.version);
    let databases = c.list_schemas().unwrap();
    assert!(databases.iter().any(|d| d == "dev"), "{databases:?}");
    assert!(databases.iter().any(|d| d == "staging"), "{databases:?}");

    let all = TableFilter::default();
    let page2 = TablePage::new(50, 50);
    let dev = c
        .query_table_in(Some("dev"), "dbo", "users", &all, page2)
        .unwrap();
    let staging = c
        .query_table_in(Some("staging"), "dbo", "users", &all, page2)
        .unwrap();
    assert_eq!(dev.columns.len(), 3);
    assert_eq!(staging.columns.len(), 6);
    assert_eq!(dev.columns[1].name, "name");
    assert_eq!(staging.columns[1].name, "login");
    // Page 2 of 50: dev has 120 rows (ids 1..=120), staging 75 (1001..).
    assert_eq!(dev.rows.len(), 50);
    assert_eq!(dev.rows[0][0], Value::Integer(51));
    assert_eq!(staging.rows.len(), 25);
    assert_eq!(staging.rows[0][0], Value::Integer(1051));
    assert_eq!(dev.order_by, vec!["id".to_string()]);
    assert_eq!(staging.order_by, vec!["uid".to_string()]);

    assert_eq!(
        c.count_table_in(Some("dev"), "dbo", "users", &all).unwrap(),
        120
    );
    assert_eq!(
        c.count_table_in(Some("staging"), "dbo", "users", &all)
            .unwrap(),
        75
    );
    let names = |db| -> Vec<String> {
        c.describe_table_in(Some(db), "dbo", "users")
            .unwrap()
            .into_iter()
            .map(|col| col.name)
            .collect()
    };
    let mut c2 = client();
    c2.connect(&profile(&target, &user), &password).unwrap();
    assert_eq!(
        c2.describe_table_in(Some("dev"), "dbo", "users")
            .unwrap()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "name", "email"]
    );
    let staging_cols = c2
        .describe_table_in(Some("staging"), "dbo", "users")
        .unwrap();
    assert_eq!(staging_cols.len(), 6);
    assert_eq!(staging_cols[3].data_type, "decimal(12,2)");
    let _ = names;

    // Indexes and the script follow the database too.
    let dev_ix = c.list_indexes_in(Some("dev"), "dbo", "users").unwrap();
    let stg_ix = c.list_indexes_in(Some("staging"), "dbo", "users").unwrap();
    assert_eq!(dev_ix[0].columns, ["id"]);
    assert_eq!(stg_ix[0].columns, ["uid"]);
    let stg_ddl = c
        .object_ddl_in(Some("staging"), "dbo", "users", &ObjectKind::Table, false)
        .unwrap();
    assert!(stg_ddl.contains("[staging].[dbo].[users]") && stg_ddl.contains("[login]"));

    // A filter travels with the database, as a bind.
    let mut filter = TableFilter::default();
    filter.conditions.push(FilterCondition::new(
        "login".to_string(),
        FilterOp::Contains,
        "stg-7".to_string(),
    ));
    let hit = c
        .query_table_in(
            Some("staging"),
            "dbo",
            "users",
            &filter,
            TablePage::new(0, 100),
        )
        .unwrap();
    assert!(!hit.rows.is_empty() && hit.rows.iter().all(|r| r[1].display().contains("stg-7")));

    // "Open in editor": the SQL the app builds from the same target and
    // order runs as is on the session, whichever database it is in, and
    // returns the grid's rows.
    let query = select_sql_in(
        c.dialect(),
        &TableTarget::new(Some("staging"), "dbo", "users"),
        &all,
        page2,
        "NULL",
        &staging.order_by,
    );
    assert!(query
        .sql
        .starts_with("SELECT * FROM [staging].[dbo].[users] ORDER BY [uid] OFFSET 50 ROWS"));
    let editor = c.query(&query.inlined(), FetchLimits::rows(100)).unwrap();
    assert_eq!(first_column(&editor), first_column(&staging));

    // Objects of a database, grouped by schema by the app.
    let objects = c.list_objects("dev").unwrap();
    let find = |schema: &str, name: &str| {
        objects
            .iter()
            .find(|o| o.owner == schema && o.name == name)
            .map(|o| o.kind.clone())
    };
    assert_eq!(find("dbo", "users"), Some(ObjectKind::Table));
    assert_eq!(find("hr", "staff"), Some(ObjectKind::Table));
    assert_eq!(find("dbo", "v_users"), Some(ObjectKind::View));
    assert_eq!(find("dbo", "nope"), None);

    // The session's own database is unchanged by all of it.
    let current = c.query("SELECT DB_NAME()", FetchLimits::rows(1)).unwrap();
    assert_eq!(current.rows[0][0], Value::Text(info.database.clone()));
    c.disconnect();
}

/// A table without a key still pages: no order is promised, and the result
/// says so by carrying no order columns.
#[test]
fn a_table_without_a_key_pages_without_an_order() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut c = client();
    c.connect(&profile(&target, &user), &password).unwrap();
    let r = c
        .query_table_in(
            Some("dev"),
            "dbo",
            "events",
            &TableFilter::default(),
            TablePage::new(1, 10),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert!(r.order_by.is_empty());
}

#[test]
fn values_come_back_in_their_own_types() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut c = client();
    c.connect(&profile(&target, &user), &password).unwrap();
    let r = c
        .query_table_in(
            Some("staging"),
            "dbo",
            "users",
            &TableFilter::default(),
            TablePage::new(0, 1),
        )
        .unwrap();
    let row = &r.rows[0];
    assert_eq!(row[0], Value::Integer(1001));
    assert_eq!(row[2], Value::Integer(1)); // bit
    assert_eq!(row[3], Value::Text("12.50".into()));
    assert_eq!(row[4], Value::Text("2020-01-02".into()));
    assert!(row[5].display().starts_with("2024-05-06 07:08:09.123"));
}

#[test]
fn a_huge_query_is_cut_at_the_row_limit_and_the_session_survives() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut c = client();
    c.connect(&profile(&target, &user), &password).unwrap();
    let r = c
        .query(
            "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b",
            FetchLimits::rows(5),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 5);
    assert!(r.truncated);
    // The cap is lifted again: the next statement is not cut at 6 rows.
    let r = c
        .query(
            "SELECT TOP (50) object_id FROM sys.all_objects",
            FetchLimits::rows(100),
        )
        .unwrap();
    assert_eq!(r.rows.len(), 50);
    assert!(!r.truncated);
}

#[test]
fn writes_wait_for_commit_and_roll_back() {
    let Some((target, user, password)) = server() else {
        return;
    };
    let mut c = client();
    c.connect(&profile(&target, &user), &password).unwrap();
    c.execute("CREATE TABLE #kt_tx (id int)").unwrap();
    c.execute_binds("INSERT INTO #kt_tx VALUES (:1)", &["7".into()])
        .unwrap();
    let count = |c: &mut RemoteClient| {
        c.query("SELECT COUNT(*) FROM #kt_tx", FetchLimits::rows(1))
            .unwrap()
            .rows[0][0]
            .clone()
    };
    assert_eq!(count(&mut c), Value::Integer(1));
    c.rollback().unwrap();
    // The temp table was created inside the transaction too.
    assert!(c
        .query("SELECT COUNT(*) FROM #kt_tx", FetchLimits::rows(1))
        .is_err());
    c.disconnect();
}
