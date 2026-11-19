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
            let mut ran_in_background = None;
            this.with_conn(1, cx, |this, background, cx| {
                ran_in_background = Some(background);
                this.on_db_response(
                    7,
                    Ok(Response::QueryDone(result_with_rows(3))),
                    background,
                    window,
                    cx,
                );
            });
            assert_eq!(ran_in_background, Some(true));

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
            this.on_db_response(3, Ok(Response::Executed(1)), false, window, cx);
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
