use super::*;

pub(super) fn sql_editor(state: &Entity<EditorState>) -> Editor {
    Editor::new(state)
        .bordered(false)
        .rounded_none()
        .flex_1()
        .pl(px(SPACE_4))
        .pr(px(SPACE_2))
        .py(px(SPACE_1))
}

impl Workbench {
    pub(super) fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            // Tab accepts the highlighted suggestion while the popup is
            // open. The editor only does that for Enter, so Tab (its
            // indent action) is caught on the way down and replayed as
            // Enter; with the popup closed it indents as usual.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let open = this
                    .active_tab()
                    .editor
                    .read(cx)
                    .completion_menu_state()
                    .open;
                if open {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(Enter {
                            secondary: false,
                            shift: false,
                        }),
                        cx,
                    );
                }
            }))
            .child(sql_editor(&self.active_tab().editor))
    }
}

impl Workbench {
    pub(super) fn save_active_script(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let tab_id = tab.id;
        let sql = tab.editor.read(cx).value().to_string();
        let saved_sql = sql.clone();
        let name = conns::script_filename(&tab.title);
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some(&name));
        let executor = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                _ => {
                    let _ = view.update(cx, |this, cx| {
                        this.push_message("Failed to open the SQL save dialog.".into());
                        cx.notify();
                    });
                    return;
                }
            };
            let display = path.display().to_string();
            let written = executor
                .spawn(async move { std::fs::write(path, sql) })
                .await;
            let _ = view.update(cx, |this, cx| {
                if written.is_ok() {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.saved_text = saved_sql;
                    }
                }
                this.push_message(match written {
                    Ok(()) => format!("SQL saved: {display}"),
                    Err(err) => format!("Failed to save SQL: {err}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn editor_text(&self, cx: &App) -> String {
        self.active_tab().editor.read(cx).value().to_string()
    }

    pub(super) fn record_run_for(&mut self, conn_id: u64, sql: &str) {
        // The connection's "Record query history" option is off.
        if self
            .conn_by_id(conn_id)
            .is_some_and(|c| c.access().skip_history)
        {
            return;
        }
        store::record_history(&mut self.history, sql, store::now_unix_secs());
        if let Err(err) = store::save_history(&self.history) {
            self.push_message_for(conn_id, format!("Failed to save query history: {err}"));
        }
    }

    pub(super) fn run(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_empty_workspace {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Connect to a database or open Demo first.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        if self.active_tab().data.is_some() {
            // Data tab "Refresh": reload whichever view is showing.
            self.load_data_view(cx);
            return;
        }
        // F5: the selection if there is one, else the whole editor, run as
        // a script (one statement after the other).
        let sql = {
            let state = self.active_tab().editor.read(cx);
            let range = state.selected_range();
            let text = state.value().to_string();
            if range.start < range.end && range.end <= text.len() {
                text[range].to_string()
            } else {
                text
            }
        };
        let statements = ora_core::sql::script_statements(&sql);
        if statements.len() < 2 {
            return self.run_sql(sql, cx);
        }
        self.conn.status = "Running script…".to_string();
        self.conn.pending_sql = Some(sql);
        self.active_tab_mut().statement_results.clear();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.conn.cancel_script = Some(cancelled.clone());
        let job = Job::Script {
            statements,
            cancelled,
        };
        self.spawn_db(job, PendingOp::Run, cx);
    }

    /// "Run Current" (Ctrl/Cmd+Enter, F9): the selection if there is one,
    /// else the statement the cursor sits in (`;`, blank line or `/` line;
    /// PL/SQL blocks stay whole).
    pub(super) fn run_current(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_empty_workspace || self.conn.session.is_none() {
            return self.run(cx);
        }
        if self.active_tab().data.is_some() {
            return self.run(cx);
        }
        let sql = self.current_statement(cx);
        self.run_sql(sql, cx);
    }

    /// The selection if there is one, else the statement the cursor sits in.
    pub(super) fn current_statement(&self, cx: &App) -> String {
        let state = self.active_tab().editor.read(cx);
        let range = state.selected_range();
        let text = state.value().to_string();
        if range.start < range.end && range.end <= text.len() {
            text[range].to_string()
        } else {
            statement_at(&text, state.cursor())
        }
    }

    pub(super) fn run_sql(&mut self, sql: String, cx: &mut Context<Self>) {
        if clean_statement(&sql).is_empty() {
            self.push_message("The editor is empty.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.conn.status = "Running query…".to_string();
        self.conn.pending_sql = Some(sql.clone());
        self.active_tab_mut().statement_results.clear();
        let job = if let Some(statements) = ora_core::sql::select_script(&sql) {
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.conn.cancel_script = Some(cancelled.clone());
            Job::SelectScript {
                statements,
                cancelled,
            }
        } else {
            Job::Run { sql }
        };
        self.spawn_db(job, PendingOp::Run, cx);
    }

    pub(super) fn explain(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_empty_workspace {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Connect to a database or open Demo first.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("The editor is empty.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.conn.status = "Generating explain plan…".to_string();
        self.spawn_db(Job::Explain { sql }, PendingOp::Explain, cx);
    }

    pub(super) fn format_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_empty_workspace {
            return;
        }
        let sql = self.editor_text(cx);
        let formatted = format_sql(&sql);
        let editor = self.active_tab().editor.clone();
        // `replace_all` keeps the undo history, so Cmd/Ctrl+Z reverts a
        // beautify done by mistake.
        editor.update(cx, |state, cx| state.replace_all(formatted, window, cx));
        cx.notify();
    }

    /// True when the active tab is a SQL editor and no dialog owns the keyboard.
    pub(super) fn sql_editor_usable(&self) -> bool {
        !self.show_empty_workspace
            && !self.settings.open
            && !self.update_check_open()
            && self.active_tab().data.is_none()
    }

    /// Cmd+/ (macOS) or Ctrl+/: toggle `-- ` comments on the cursor line or selected
    /// lines. One undo step; the selection stays on the same text.
    pub(super) fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sql_editor_usable() {
            return;
        }
        let editor = self.active_tab().editor.clone();
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let (text, range) = {
            let state = editor.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let Some(edit) = ora_core::sql::toggle_line_comment(&text, range) else {
            return;
        };
        editor.update(cx, |state, cx| {
            state.set_selected_range(edit.range.clone(), cx);
            state.replace(edit.text.clone(), window, cx);
            state.set_selected_range(edit.selection.clone(), cx);
        });
        cx.notify();
    }

    /// Soft-cancel only: Oracle offers no cancel handle through the thin
    /// driver call in use, so the worker keeps running the operation until
    /// the database answers. Stop therefore stops *waiting*: the response is
    /// ignored when it arrives, but the connection stays busy until then so
    /// the user cannot start another operation (a second write) while the
    /// first may still complete. The UI must never claim a real cancel.
    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.conn.busy
            || self.conn.abandoned
            || matches!(self.conn.pending_op, Some(PendingOp::ApplyEdits))
        {
            return;
        }
        if let Some(cancelled) = self.conn.cancel_script.take() {
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.conn.pending_sql = None;
        if self.conn.pending_op.is_none() {
            // Nothing reached the worker yet (waiting for the production
            // confirmation): drop the pending dispatch and free the UI.
            self.conn.op_generation += 1;
            self.conn.busy = false;
            self.conn.status = "Stopped.".to_string();
            cx.notify();
            return;
        }
        self.conn.abandoned = true;
        self.conn.status =
            "Stopped waiting. The database is still working; busy until it answers.".to_string();
        self.push_message(
            "Stopped waiting for the database. The operation is still running on the server and its response will be ignored. You can run something new once it finishes.".to_string(),
        );
        cx.notify();
    }
}
