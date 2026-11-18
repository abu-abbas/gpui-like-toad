use super::*;

pub(super) fn sql_editor(state: &Entity<EditorState>) -> Editor {
    Editor::new(state)
        .bordered(false)
        .rounded_none()
        .flex_1()
        .px(px(SPACE_2))
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
        let sql = tab.editor.read(cx).value().to_string();
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
                        this.push_message("Gagal membuka dialog simpan SQL.".into());
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
                this.push_message(match written {
                    Ok(()) => format!("SQL tersimpan: {display}"),
                    Err(err) => format!("Gagal menyimpan SQL: {err}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn editor_text(&self, cx: &App) -> String {
        self.active_tab().editor.read(cx).value().to_string()
    }

    pub(super) fn record_run(&mut self, sql: &str) {
        store::record_history(&mut self.history, sql, store::now_unix_secs());
        if let Err(err) = store::save_history(&self.history) {
            self.push_message(format!("Gagal menyimpan history: {err}"));
        }
    }

    pub(super) fn run(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_empty_workspace {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        if self.active_tab().data.is_some() {
            // Data tab "Refresh": reload whichever view is showing.
            self.load_data_view(cx);
            return;
        }
        let sql = self.editor_text(cx);
        self.run_sql(sql, cx);
    }

    /// "Run Current": the selection if there is one, else the statement
    /// the cursor sits in (statements end at `;`).
    pub(super) fn run_current(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.show_empty_workspace || self.conn.session.is_none() {
            return self.run(cx);
        }
        if self.active_tab().data.is_some() {
            return self.run(cx);
        }
        let editor = self.active_tab().editor.clone();
        let sql = {
            let state = editor.read(cx);
            let range = state.selected_range();
            let text = state.value().to_string();
            if range.start < range.end && range.end <= text.len() {
                text[range].to_string()
            } else {
                statement_at(&text, state.cursor())
            }
        };
        self.run_sql(sql, cx);
    }

    fn run_sql(&mut self, sql: String, cx: &mut Context<Self>) {
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.conn.status = "Menjalankan...".to_string();
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
            self.push_message("Belum terhubung: Connect atau Demo dulu.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        let sql = self.editor_text(cx);
        if clean_statement(&sql).is_empty() {
            self.push_message("Editor kosong.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        }
        self.conn.status = "Menjelaskan plan...".to_string();
        self.spawn_db(Job::Explain { sql }, PendingOp::Explain, cx);
    }

    pub(super) fn format_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_empty_workspace {
            return;
        }
        let sql = self.editor_text(cx);
        let formatted = format_sql(&sql);
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(formatted, window, cx));
        cx.notify();
    }

    /// Soft-cancel only: bump the generation so the in-flight response is
    /// dropped when it arrives, and clear the busy state immediately. The
    /// worker thread itself stays blocked until Oracle returns, and a job
    /// sent afterwards queues behind it in the worker channel. This is NOT
    /// true cancellation — Oracle offers no cancel handle through the thin
    /// driver call in use — and the UI must never claim otherwise.
    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.conn.busy || matches!(self.conn.pending_op, Some(PendingOp::ApplyEdits)) {
            return;
        }
        if let Some(cancelled) = self.conn.cancel_script.take() {
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.conn.op_generation += 1;
        self.conn.pending_op = None;
        self.conn.pending_sql = None;
        self.conn.busy = false;
        self.conn.status = "Dihentikan (respons dibuang).".to_string();
        self.push_message(
            "Stop: operasi database yang sedang berjalan tidak dapat dibatalkan; responsnya akan diabaikan saat tiba.".to_string(),
        );
        cx.notify();
    }
}
