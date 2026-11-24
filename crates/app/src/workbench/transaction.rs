//! Open transactions: which statements start or end one, and how the UI
//! shows it. Every engine in Kilat waits for Commit (the drivers open a
//! transaction on the first write), so the markers only appear once a
//! connection really holds uncommitted changes:
//!
//! - status bar: a yellow "● 3 uncommitted" badge, click for Commit or
//!   Rollback;
//! - toolbar: Commit and Rollback in colour with a dot (dimmed otherwise,
//!   but still clickable because tracking is best effort);
//! - sidebar row and tabs: a small yellow dot;
//! - Disconnect, closing the window and Quit first ask Commit, Rollback
//!   or Cancel.

use super::*;
use ora_core::sql::{statement_kind, StatementKind};

/// What one statement does to the session's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TxEffect {
    /// Reads, or session settings.
    None,
    /// A change that waits for Commit.
    Write,
    /// COMMIT / ROLLBACK typed in the editor, or DDL on an engine where
    /// DDL commits implicitly (Oracle, MySQL).
    Ends,
}

/// First two words of `sql` after comments, uppercased.
fn words(sql: &str) -> Vec<String> {
    let mut s = clean_statement(sql);
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.find('\n').map_or("", |ix| &rest[ix + 1..]);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.find("*/").map_or("", |ix| &rest[ix + 2..]);
        } else {
            break;
        }
    }
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .take(2)
        .map(str::to_ascii_uppercase)
        .collect()
}

/// Best-effort classification of a statement that ran without rows.
pub(super) fn tx_effect(kind: DbKind, sql: &str) -> TxEffect {
    let words = words(sql);
    let first = words.first().map_or("", String::as_str);
    let second = words.get(1).map_or("", String::as_str);
    match first {
        "COMMIT" => return TxEffect::Ends,
        // ROLLBACK TO SAVEPOINT keeps the transaction open.
        "ROLLBACK" if second == "TO" => return TxEffect::None,
        "ROLLBACK" => return TxEffect::Ends,
        // PostgreSQL's COMMIT spelling.
        "END" if kind == DbKind::Postgres => return TxEffect::Ends,
        "SAVEPOINT" | "SET" | "EXPLAIN" | "SHOW" | "DESCRIBE" | "DESC" | "USE" => {
            return TxEffect::None
        }
        _ => {}
    }
    match statement_kind(sql) {
        StatementKind::Select | StatementKind::Empty => TxEffect::None,
        StatementKind::Ddl if matches!(kind, DbKind::Oracle | DbKind::Mysql) => TxEffect::Ends,
        _ => TxEffect::Write,
    }
}

impl ConnState {
    /// Record what a finished operation did to the transaction.
    pub(super) fn note_response(
        &mut self,
        pending: Option<&PendingOp>,
        resp: &Response,
        sql: Option<&str>,
        tab: Option<u64>,
    ) {
        match (pending, resp) {
            (Some(PendingOp::Run), Response::Executed(_)) => match sql {
                Some(sql) => self.note_statement(sql, tab),
                None => self.note_write(1, tab),
            },
            (Some(PendingOp::Run), Response::ScriptRan(steps)) => {
                for step in steps {
                    if matches!(step.outcome, Ok(crate::worker::StepOk::Affected(_))) {
                        self.note_statement(&step.sql, tab);
                    }
                }
            }
            (Some(PendingOp::ImportCsv { .. }), Response::Imported(_))
            | (
                Some(PendingOp::ObjectOp {
                    uncommitted: true, ..
                }),
                Response::ObjectOp(_),
            ) => self.note_write(1, tab),
            _ => {}
        }
    }

    /// Writes waiting for Commit (0 when none).
    pub(super) fn pending_changes(&self) -> u32 {
        if self.uncommitted {
            self.uncommitted_changes.max(1)
        } else {
            0
        }
    }
}

/// What to do once the open transactions were committed or rolled back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TxAfter {
    Disconnect(u64),
    CloseWindow,
    Quit,
}

/// The "uncommitted changes" dialog.
pub(super) struct TxPrompt {
    /// Connections with uncommitted changes.
    conns: Vec<u64>,
    after: TxAfter,
}

/// Commit or Rollback sent from the dialog, waiting for the answers.
pub(super) struct TxFinish {
    waiting: HashSet<u64>,
    after: TxAfter,
    failed: bool,
}

/// Every open workbench window, so Quit can ask each one.
#[derive(Default)]
struct OpenWorkbenches(Vec<gpui_kit::WeakEntity<Workbench>>);

impl gpui_kit::Global for OpenWorkbenches {}

/// Quit (Cmd+Q, menu): ask about uncommitted changes in each window
/// first; quit when there are none.
pub fn request_quit(cx: &mut App) {
    let views: Vec<_> = cx
        .try_global::<OpenWorkbenches>()
        .map(|open| open.0.clone())
        .unwrap_or_default();
    for view in views.into_iter().filter_map(|weak| weak.upgrade()) {
        let (asked, handle) = view.update(cx, |this, cx| {
            (this.guard_tx(TxAfter::Quit, cx), this.window_handle)
        });
        if asked {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    cx.quit();
}

impl Workbench {
    /// Called from `Workbench::new`: register for Quit and ask before the
    /// window closes with uncommitted changes.
    pub fn watch_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        cx.default_global::<OpenWorkbenches>()
            .0
            .retain(|w| w.upgrade().is_some());
        cx.default_global::<OpenWorkbenches>().0.push(weak.clone());
        window.on_window_should_close(cx, move |_, cx| {
            weak.upgrade().is_none_or(|view| {
                view.update(cx, |this, cx| !this.guard_tx(TxAfter::CloseWindow, cx))
            })
        });
    }

    /// Connections `after` would close that hold uncommitted changes.
    fn conns_with_open_tx(&self, after: &TxAfter) -> Vec<u64> {
        std::iter::once(&self.conn)
            .chain(self.parked.iter())
            .filter(|c| c.session.is_some() && c.uncommitted)
            .filter(|c| !matches!(after, TxAfter::Disconnect(id) if *id != c.id))
            .map(|c| c.id)
            .collect()
    }

    /// Show the Commit / Rollback / Cancel dialog when `after` would lose
    /// uncommitted changes. True when it was shown (the caller stops).
    pub(super) fn guard_tx(&mut self, after: TxAfter, cx: &mut Context<Self>) -> bool {
        if let Some(finish) = &self.tx_finish {
            // Already committing for this; let that finish. Never hang on
            // an answer that cannot come any more.
            if finish
                .waiting
                .iter()
                .any(|id| self.conn_by_id(*id).is_some_and(|c| c.busy))
            {
                return true;
            }
            self.tx_finish = None;
        }
        let conns = self.conns_with_open_tx(&after);
        if conns.is_empty() {
            return false;
        }
        self.tx_prompt = Some(TxPrompt { conns, after });
        cx.notify();
        true
    }

    fn close_tx_prompt(&mut self, cx: &mut Context<Self>) {
        self.tx_prompt = None;
        cx.notify();
    }

    /// Dialog answer: commit (or roll back) every listed connection, then
    /// carry on with what was asked.
    fn finish_tx_prompt(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.tx_prompt.take() else {
            return;
        };
        let mut waiting = HashSet::new();
        for id in prompt.conns {
            if self
                .conn_by_id(id)
                .is_some_and(|c| c.session.is_some() && c.db.is_some() && !c.busy)
            {
                let (job, op) = if commit {
                    (Job::Commit, PendingOp::Commit)
                } else {
                    (Job::Rollback, PendingOp::Rollback)
                };
                waiting.insert(id);
                self.spawn_db_for(id, job, op, cx);
            }
        }
        if waiting.is_empty() {
            self.run_tx_after(prompt.after, cx);
        } else {
            self.tx_finish = Some(TxFinish {
                waiting,
                after: prompt.after,
                failed: false,
            });
        }
        cx.notify();
    }

    /// A Commit or Rollback on `cid` finished (`ok`) or failed. When it
    /// was sent from the dialog and it is the last one, carry on.
    pub(super) fn tx_step_done(&mut self, cid: u64, ok: bool, cx: &mut Context<Self>) {
        let Some(finish) = self.tx_finish.as_mut() else {
            return;
        };
        if !finish.waiting.remove(&cid) {
            return;
        }
        finish.failed |= !ok;
        if !finish.waiting.is_empty() {
            return;
        }
        let finish = self.tx_finish.take().expect("checked above");
        if finish.failed {
            self.push_message(
                "Commit or Rollback failed, so nothing was closed. See the message above.".into(),
            );
            cx.notify();
            return;
        }
        self.run_tx_after(finish.after, cx);
    }

    fn run_tx_after(&mut self, after: TxAfter, cx: &mut Context<Self>) {
        match after {
            TxAfter::Disconnect(id) => self.disconnect_after_tx(id, cx),
            TxAfter::CloseWindow => {
                let handle = self.window_handle;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                });
            }
            // Other windows may still ask.
            TxAfter::Quit => cx.defer(request_quit),
        }
    }

    /// True when tab `tab` made changes that are not committed yet.
    pub(super) fn tab_has_open_tx(&self, tab: &QueryTab) -> bool {
        tab.conn
            .and_then(|id| self.conn_by_id(id))
            .is_some_and(|c| c.uncommitted && c.uncommitted_tabs.contains(&tab.id))
    }

    /// True when connection `id` holds uncommitted changes.
    pub(super) fn conn_has_open_tx(&self, id: u64) -> bool {
        self.conn_by_id(id)
            .is_some_and(|c| c.session.is_some() && c.uncommitted)
    }

    /// Status bar badge "● 3 uncommitted"; click for Commit / Rollback.
    pub(super) fn render_uncommitted_badge(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let changes = self.conn.pending_changes();
        if changes == 0 || self.conn.session.is_none() {
            return None;
        }
        let busy = self.conn.busy;
        let view = cx.entity();
        let noun = if changes == 1 { "change" } else { "changes" };
        Some(
            Button::new("status-uncommitted")
                .ghost()
                .xsmall()
                .flex_none()
                .tooltip(format!(
                    "{changes} {noun} since the last Commit or Rollback. Click to Commit or Roll back."
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_1))
                        .px(px(SPACE_1))
                        .rounded(px(RADIUS_SM))
                        .bg(theme.warning.opacity(0.15))
                        .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                        .line_height(crate::theme::ui_size(theme, LEADING_CAPTION))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.warning)
                        .child(status_dot(theme.warning).size(px(6.)))
                        .child(format!("{changes} uncommitted")),
                )
                .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomLeft, move |menu, _, _| {
                    let commit_view = view.clone();
                    let rollback_view = view.clone();
                    menu.item(
                        PopupMenuItem::new("Commit")
                            .icon(Icon::new(IconName::Check))
                            .disabled(busy)
                            .on_click(move |_, _, cx: &mut App| {
                                commit_view.update(cx, |this, cx| this.commit(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Rollback")
                            .icon(Icon::new(IconName::Undo2))
                            .disabled(busy)
                            .on_click(move |_, _, cx: &mut App| {
                                rollback_view.update(cx, |this, cx| this.rollback(cx));
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    /// The dialog shown before Disconnect, closing the window or Quit.
    pub(super) fn render_tx_prompt(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(prompt) = self.tx_prompt.as_ref() else {
            return div().into_any_element();
        };
        let lines: Vec<String> = prompt
            .conns
            .iter()
            .filter_map(|id| self.conn_by_id(*id))
            .map(|c| {
                let n = c.pending_changes();
                format!(
                    "{}: {n} {}",
                    c.label(),
                    if n == 1 { "change" } else { "changes" }
                )
            })
            .collect();
        let busy = prompt
            .conns
            .iter()
            .any(|id| self.conn_by_id(*id).is_some_and(|c| c.busy));
        let (heading, action) = match prompt.after {
            TxAfter::Disconnect(_) => ("Uncommitted changes", "before disconnecting"),
            TxAfter::CloseWindow => ("Uncommitted changes", "before closing the window"),
            TxAfter::Quit => ("Uncommitted changes", "before quitting"),
        };
        let mut text = format!(
            "Commit or roll back {action}? Closing the session without either rolls the changes back."
        );
        if busy {
            text.push_str(" A query is still running; wait for it or Stop it first.");
        }
        let card = div()
            .id("tx-prompt")
            .flex()
            .flex_col()
            .w(px(460.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_tx_prompt(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2))
                            .child(icon(IconName::TriangleAlert, 18., theme.warning))
                            .child(
                                div()
                                    .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                                    .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(heading),
                            ),
                    )
                    .children(lines.into_iter().map(|line| {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(SPACE_2))
                            .text_size(crate::theme::ui_size(theme, FONT_BODY))
                            .line_height(crate::theme::ui_size(theme, LEADING_BODY))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(status_dot(theme.warning))
                            .child(line)
                    }))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_BODY))
                            .line_height(crate::theme::ui_size(theme, LEADING_BODY))
                            .text_color(theme.muted_foreground)
                            .child(text),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .bg(theme.muted)
                    .child(
                        Button::new("tx-cancel")
                            .flex_1()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_tx_prompt(cx))),
                    )
                    .child(
                        Button::new("tx-rollback")
                            .flex_1()
                            .label("Rollback")
                            .danger()
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.finish_tx_prompt(false, cx)),
                            ),
                    )
                    .child(
                        Button::new("tx-commit")
                            .flex_1()
                            .label("Commit")
                            .primary()
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.finish_tx_prompt(true, cx)),
                            ),
                    ),
            );
        div()
            .id("tx-prompt-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(160.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }
}

/// Small yellow dot for a tab or sidebar row with uncommitted changes.
pub(super) fn tx_dot(
    id: impl Into<gpui_kit::ElementId>,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .size(px(7.))
        .rounded_full()
        .bg(theme.warning)
        .tooltip(|window, cx| {
            gpui_kit::component::tooltip::Tooltip::new("Uncommitted changes: Commit or Rollback")
                .build(window, cx)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    fn session() -> DbInfo {
        DbInfo {
            user: "ALPHA".to_string(),
            database: "TESTDB".to_string(),
            version: "test".to_string(),
        }
    }

    /// Disconnect with uncommitted changes asks first; a failed Rollback
    /// from the dialog keeps the connection, a Commit closes it.
    #[gpui_kit::test]
    fn disconnect_asks_then_closes_after_commit(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(Workbench::new);
        window
            .update(cx, |this, _, cx| {
                this.conn.session = Some(session());
                assert!(!this.guard_tx(TxAfter::Disconnect(1), cx));
                this.conn.note_write(2, None);
                assert!(this.guard_tx(TxAfter::Disconnect(1), cx));
                assert!(this.tx_prompt.is_some());
                this.close_tx_prompt(cx);

                this.tx_finish = Some(TxFinish {
                    waiting: HashSet::from([1]),
                    after: TxAfter::Disconnect(1),
                    failed: false,
                });
                this.tx_step_done(1, false, cx);
                assert!(this.tx_finish.is_none());
                assert!(this.conn.session.is_some() && this.conn.uncommitted);

                this.tx_finish = Some(TxFinish {
                    waiting: HashSet::from([1]),
                    after: TxAfter::Disconnect(1),
                    failed: false,
                });
                this.on_committed(1, cx);
                assert!(this.tx_finish.is_none());
                assert!(this.conn.session.is_none());
                assert!(!this.conn.uncommitted);
            })
            .unwrap();
    }

    #[test]
    fn classifies_statements() {
        use TxEffect::*;
        let o = DbKind::Oracle;
        assert_eq!(tx_effect(o, "update t set a = 1"), Write);
        assert_eq!(tx_effect(o, "  -- note\n/* x */ DELETE FROM t;"), Write);
        assert_eq!(tx_effect(o, "begin p; end;"), Write);
        assert_eq!(tx_effect(o, "select 1 from dual"), None);
        assert_eq!(tx_effect(o, "commit;"), Ends);
        assert_eq!(tx_effect(o, "ROLLBACK"), Ends);
        assert_eq!(tx_effect(o, "rollback to savepoint a"), None);
        assert_eq!(tx_effect(o, "savepoint a"), None);
        assert_eq!(tx_effect(o, "create table x (a number)"), Ends);
        assert_eq!(tx_effect(DbKind::Mysql, "alter table x add b int"), Ends);
        assert_eq!(tx_effect(DbKind::Postgres, "create table x (a int)"), Write);
        assert_eq!(tx_effect(DbKind::Postgres, "end"), Ends);
        assert_eq!(tx_effect(o, "end"), Write);
    }

    #[test]
    fn counts_writes_and_clears() {
        let mut conn = ConnState::new(1);
        conn.note_statement("insert into t values (1)", Some(4));
        conn.note_statement("select * from t", Some(5));
        conn.note_write(3, Some(6));
        assert!(conn.uncommitted);
        assert_eq!(conn.pending_changes(), 4);
        assert!(conn.uncommitted_tabs.contains(&4) && conn.uncommitted_tabs.contains(&6));
        assert!(!conn.uncommitted_tabs.contains(&5));
        conn.note_statement("create table x (a number)", None);
        assert!(!conn.uncommitted);
        assert_eq!(conn.pending_changes(), 0);
        assert!(conn.uncommitted_tabs.is_empty());
    }
}
