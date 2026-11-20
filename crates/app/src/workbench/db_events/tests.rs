use super::*;
use gpui_kit::TestAppContext;
use ora_core::{ColumnInfo, Value};

fn session(user: &str) -> DbInfo {
    DbInfo {
        user: user.to_string(),
        database: "TESTDB".to_string(),
        version: "test".to_string(),
    }
}

fn result_with_rows(n: usize) -> QueryResult {
    QueryResult {
        columns: vec![ColumnInfo {
            name: "ID".to_string(),
            data_type: "NUMBER".to_string(),
        }],
        rows: (0..n).map(|i| vec![Value::Integer(i as i128)]).collect(),
        ..Default::default()
    }
}

/// A query started on connection A finishes after the user switched to
/// connection B: the result lands in A's tab and A's state, while B stays
/// the active connection with its tab and status untouched.
#[gpui_kit::test]
fn query_finishes_after_switching_connection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, window, cx| {
            // Connection A (id 1) runs a query from its own tab.
            this.conn.session = Some(session("ALPHA"));
            this.add_tab(window, cx);
            let a_tab = this.active_tab().id;
            assert_eq!(this.active_tab().conn, Some(1));
            this.conn.op_generation = 7;
            this.conn.pending_op = Some(PendingOp::Run);
            this.conn.pending_tab = Some(a_tab);
            this.conn.busy = true;

            // The user opens connection B (id 2) and a tab on it while A
            // is still running.
            let mut b = ConnState::new(2);
            b.session = Some(session("BETA"));
            b.status = "B is idle.".to_string();
            let a = std::mem::replace(&mut this.conn, b);
            this.parked.push(a);
            this.add_tab(window, cx);
            let b_tab = this.active_tab().id;
            assert_eq!(this.active_tab().conn, Some(2));

            // A's response arrives.
            this.on_db_response(
                1,
                7,
                Ok(Response::QueryDone(result_with_rows(3))),
                window,
                cx,
            );

            // B is still the active connection, on its own tab.
            assert_eq!(this.conn.id, 2);
            assert_eq!(this.active_tab().id, b_tab);
            assert_eq!(this.conn.status, "B is idle.");
            assert!(this.active_tab().last_result.is_none());

            // A got the result and is idle again.
            let a = this.conn_by_id(1).expect("A stays open");
            assert!(!a.busy);
            assert!(a.pending_op.is_none());
            assert!(a.status.starts_with("3 rows in "), "{}", a.status);
            let tab = this
                .tabs
                .iter()
                .find(|t| t.id == a_tab)
                .expect("A's tab stays open");
            assert_eq!(tab.last_result.as_ref().map(|r| r.row_count()), Some(3));
            assert!(matches!(tab.results_tab, ResultsTab::Results));
        })
        .unwrap();
}

/// Stop only stops waiting: the connection stays busy (so a second write
/// cannot be sent) until the worker answers, and that answer is ignored.
#[gpui_kit::test]
fn stop_keeps_connection_busy_until_worker_answers(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, window, cx| {
            this.conn.session = Some(session("ALPHA"));
            this.add_tab(window, cx);
            this.conn.op_generation = 3;
            this.conn.pending_op = Some(PendingOp::Run);
            this.conn.busy = true;

            this.stop(cx);
            assert!(this.conn.busy, "still busy after Stop");
            assert!(this.conn.abandoned);
            // A second Stop is a no-op.
            this.stop(cx);
            assert!(this.conn.busy);

            // The worker answers; the result is dropped, a write is noted.
            this.on_db_response(this.conn.id, 3, Ok(Response::Executed(1)), window, cx);
            assert!(!this.conn.busy);
            assert!(!this.conn.abandoned);
            assert!(this.conn.pending_op.is_none());
            assert!(this.conn.uncommitted);
            assert!(this.active_tab().last_result.is_none());
        })
        .unwrap();
}

/// Stop while only waiting for the production confirmation frees the UI:
/// nothing reached the worker.
#[gpui_kit::test]
fn stop_before_dispatch_frees_connection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, window, cx| {
            this.conn.session = Some(session("ALPHA"));
            this.add_tab(window, cx);
            this.conn.busy = true;
            this.stop(cx);
            assert!(!this.conn.busy);
            assert!(!this.conn.abandoned);
        })
        .unwrap();
}

/// Looking up a parked connection (its generation, its state) never makes
/// it the active one, and a stale or unknown operation is not current.
#[gpui_kit::test]
fn parked_connection_is_reached_without_activating_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, _window, _cx| {
            this.conn.op_generation = 4;
            let mut b = ConnState::new(2);
            b.op_generation = 9;
            let a = std::mem::replace(&mut this.conn, b);
            this.parked.push(a);

            assert!(this.op_is_current(2, 9));
            assert!(this.op_is_current(1, 4));
            assert!(!this.op_is_current(1, 5), "superseded operation");
            assert!(!this.op_is_current(3, 0), "unknown connection");

            this.conn_mut(1).expect("A is parked").status = "touched".to_string();
            assert_eq!(this.conn.id, 2, "B stays active");
            assert_eq!(this.conn_by_id(1).unwrap().status, "touched");
            assert!(this.conn_mut(7).is_none());
        })
        .unwrap();
}

/// A commit and a failure finishing on a parked connection update only that
/// connection; the active one keeps its state and stays active.
#[gpui_kit::test]
fn background_commit_and_failure_leave_active_connection_alone(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, window, cx| {
            this.conn.session = Some(session("ALPHA"));
            this.conn.uncommitted = true;
            this.conn.op_generation = 3;
            this.conn.pending_op = Some(PendingOp::Commit);
            this.conn.busy = true;
            let mut b = ConnState::new(2);
            b.session = Some(session("BETA"));
            b.status = "B is idle.".to_string();
            let a = std::mem::replace(&mut this.conn, b);
            this.parked.push(a);

            this.on_db_response(1, 3, Ok(Response::Committed), window, cx);
            let a = this.conn_by_id(1).unwrap();
            assert!(!a.uncommitted && !a.busy);
            assert_eq!(a.status, "Commit succeeded.");

            let a = this.conn_mut(1).unwrap();
            a.op_generation = 4;
            a.pending_op = Some(PendingOp::Rollback);
            a.busy = true;
            this.on_db_response(1, 4, Ok(Response::Failed("boom".into())), window, cx);
            assert_eq!(this.conn_by_id(1).unwrap().status, "Error (see Messages).");
            assert!(!this.conn_by_id(1).unwrap().busy);

            assert_eq!(this.conn.id, 2);
            assert_eq!(this.conn.status, "B is idle.");
        })
        .unwrap();
}

/// A schema list and a lost connection on a parked connection touch only
/// that connection and its own catalog, never the active one's.
#[gpui_kit::test]
fn background_schemas_and_connection_loss_stay_on_their_connection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let window = cx.add_window(Workbench::new);
    window
        .update(cx, |this, window, cx| {
            this.conn.session = Some(session("ALPHA"));
            this.conn.op_generation = 5;
            this.conn.pending_op = Some(PendingOp::ListSchemas);
            this.conn.busy = true;
            let mut b = ConnState::new(2);
            b.session = Some(session("BETA"));
            b.schemas = vec!["BETA".to_string()];
            let a = std::mem::replace(&mut this.conn, b);
            this.parked.push(a);

            let schemas = vec!["ALPHA".to_string(), "HR".to_string()];
            this.on_db_response(1, 5, Ok(Response::Schemas(schemas)), window, cx);
            let a = this.conn_by_id(1).unwrap();
            assert_eq!(a.schemas, ["ALPHA", "HR"]);
            assert_eq!(a.current_schema.as_deref(), Some("ALPHA"));
            assert_eq!(this.conn.schemas, ["BETA"], "B untouched");

            let a = this.conn_mut(1).unwrap();
            a.op_generation = 6;
            a.pending_op = Some(PendingOp::Run);
            a.busy = true;
            this.on_db_response(
                1,
                6,
                Ok(Response::ConnectionLost("gone".into())),
                window,
                cx,
            );
            let a = this.conn_by_id(1).unwrap();
            assert!(a.connection_lost && a.session.is_none() && a.schemas.is_empty());
            assert!(this.conn.session.is_some(), "B stays connected");
            assert_eq!(this.conn.id, 2);
        })
        .unwrap();
}
