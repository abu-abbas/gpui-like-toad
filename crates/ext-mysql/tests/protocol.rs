//! End to end: the real extension program, driven through `RemoteClient`.
//! The database part needs a server: set `KILAT_TEST_MYSQL` to
//! `host:port/database;user;password` (CI has none, so it is skipped there).

use std::path::PathBuf;

use ora_core::extension_client::RemoteClient;
use ora_core::{ConnectionProfile, DbClient, DbKind, FetchLimits, Value};

fn client() -> RemoteClient {
    RemoteClient::new(
        PathBuf::from(env!("CARGO_BIN_EXE_kilat-ext-mysql")),
        "mysql",
        DbKind::Mysql,
        "test",
    )
}

fn profile(target: &str, user: &str) -> ConnectionProfile {
    ConnectionProfile {
        name: "t".into(),
        kind: DbKind::Mysql,
        username: user.into(),
        connect_string: target.into(),
        ..ConnectionProfile::default()
    }
}

#[test]
fn the_program_answers_the_handshake_and_reports_errors() {
    let mut c = client();
    // Nothing listens on port 1: the handshake works, the connect fails
    // with the driver's own message.
    let err = c.connect(&profile("127.0.0.1:1/db", "u"), "p").unwrap_err();
    assert!(err.0.contains("MySQL"), "{err}");
    // And the program is still alive for the next call.
    let err = c.connect(&profile("", "u"), "p").unwrap_err();
    assert!(err.0.contains("host"), "{err}");
}

#[test]
fn queries_and_edits_work_through_the_protocol() {
    let Some(spec) = std::env::var("KILAT_TEST_MYSQL").ok() else {
        eprintln!("KILAT_TEST_MYSQL not set: skipped");
        return;
    };
    let mut parts = spec.split(';');
    let (target, user, password) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let mut c = client();
    let info = c.connect(&profile(target, user), password).unwrap();
    assert!(
        info.version.contains("MySQL") || info.version.contains("MariaDB"),
        "{}",
        info.version
    );
    c.execute("CREATE TEMPORARY TABLE kt_proto (id int, name varchar(20))")
        .unwrap();
    c.execute_binds(
        "INSERT INTO kt_proto VALUES (:1, :2)",
        &["7".into(), "x'y".into()],
    )
    .unwrap();
    let r = c
        .query_binds(
            "SELECT id, name FROM kt_proto WHERE id = :1",
            &["7".into()],
            FetchLimits::rows(10),
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::Integer(7), Value::Text("x'y".into())]]
    );
    assert!(c.list_schemas().unwrap().iter().any(|s| s == "kt_main"));
    c.rollback().unwrap();
    c.disconnect();
}
