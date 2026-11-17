//! Several connections open at once (say DEV and PROD side by side).
//!
//! Each connection is a [`ConnState`] with its own worker thread. The
//! active one lives in `Workbench::conn`, the others in `Workbench::parked`;
//! activating a connection swaps it in, so the explorer, toolbar, status
//! bar and autocomplete always show the active one. Every tab is bound to
//! the connection it runs on, and picking a tab activates its connection.
//!
//! A response that arrives for a parked connection is applied with that
//! connection swapped in for the duration of the handler
//! ([`Workbench::with_conn`]), so the handlers never need to know which
//! connection is active.

use super::*;

/// The "save scripts before disconnect?" dialog: which connection, and
/// the SQL tabs (id, title) that would be closed with text in them.
pub(super) struct DisconnectPrompt {
    id: u64,
    label: String,
    scripts: Vec<(u64, String)>,
}

impl Workbench {
    /// Connections that are connected (or connecting), active one included.
    pub(super) fn open_conn_count(&self) -> usize {
        usize::from(self.conn.is_open()) + self.parked.iter().filter(|c| c.is_open()).count()
    }

    fn parked_ix(&self, id: u64) -> Option<usize> {
        self.parked.iter().position(|c| c.id == id)
    }

    /// The connection with this id, active or parked.
    pub(super) fn conn_by_id(&self, id: u64) -> Option<&ConnState> {
        if self.conn.id == id {
            Some(&self.conn)
        } else {
            self.parked.iter().find(|c| c.id == id)
        }
    }

    /// Swap `parked[ix]` with the active connection, autocomplete catalog
    /// included (the completion provider shares `self.catalog`, so the
    /// live catalog moves in and out of it). Calling it twice with the same
    /// index restores the original state.
    fn swap_active(&mut self, ix: usize) {
        std::mem::swap(&mut self.conn, &mut self.parked[ix]);
        let mut live = self.catalog.borrow_mut();
        std::mem::swap(&mut *live, &mut self.conn.catalog);
        std::mem::swap(&mut self.conn.catalog, &mut self.parked[ix].catalog);
    }

    /// Make connection `id` the active one. The previous active connection
    /// is parked, or dropped when it holds no session and is idle (a
    /// failed connect, or the empty start state).
    pub(super) fn activate_conn(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.conn.id == id {
            return;
        }
        let Some(ix) = self.parked_ix(id) else {
            return;
        };
        self.swap_active(ix);
        let prev = &self.parked[ix];
        if !prev.is_open() && !prev.busy {
            self.parked.remove(ix);
        }
        cx.notify();
    }

    /// Run `f` with connection `id` active. Used for responses: one that
    /// arrives for a parked connection is applied to that connection, then
    /// the user's active connection is restored. `f` is told whether it
    /// runs in the background, so it can skip UI moves (focus, tab
    /// switches) that only make sense for what the user is looking at.
    /// Responses for a connection that is gone are dropped.
    pub(super) fn with_conn(
        &mut self,
        id: u64,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, bool, &mut Context<Self>),
    ) {
        if self.conn.id == id {
            f(self, false, cx);
            return;
        }
        let Some(ix) = self.parked_ix(id) else {
            return;
        };
        let user_conn = self.conn.id;
        self.swap_active(ix);
        f(self, true, cx);
        if let Some(ix) = self.parked_ix(user_conn) {
            self.swap_active(ix);
        }
        // The handler may have opened a tab for its connection (View
        // Script): follow it.
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Activate the connection of the active tab, if it is open.
    pub(super) fn follow_tab_conn(&mut self, cx: &mut Context<Self>) {
        if self.show_home {
            return;
        }
        if let Some(id) = self.tabs.get(self.active_tab).and_then(|t| t.conn) {
            if id != self.conn.id && self.parked_ix(id).is_some() {
                self.activate_conn(id, cx);
            }
        }
    }

    /// The active tab belongs to a connection that has been closed.
    pub(super) fn active_tab_orphaned(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .and_then(|t| t.conn)
            .is_some_and(|id| self.conn_by_id(id).is_none_or(|c| !c.is_open()))
    }

    /// An open connection for this profile (or the demo database), so
    /// opening it again switches to it instead of logging in twice.
    pub(super) fn find_open_conn(&self, profile_ix: Option<usize>, demo: bool) -> Option<u64> {
        let matches = |c: &ConnState| {
            c.is_open()
                && c.last_connection.as_ref().is_some_and(|l| {
                    if demo {
                        l.demo
                    } else {
                        !l.demo && profile_ix.is_some() && l.profile_ix == profile_ix
                    }
                })
        };
        std::iter::once(&self.conn)
            .chain(self.parked.iter())
            .find(|c| matches(c))
            .map(|c| c.id)
    }

    /// Before a connect: keep the active connection open (parked) and give
    /// the new one a fresh slot with its own worker. A disconnected active
    /// slot is reused, so tabs bound to it work again after a reconnect.
    pub(super) fn prepare_new_conn(&mut self) {
        if !self.conn.is_open() && !self.conn.busy {
            return;
        }
        // Connecting from a tab of the old connection: show Home while
        // this one connects, then open a tab for it.
        let from_tab = !self.show_home
            && self
                .tabs
                .get(self.active_tab)
                .is_some_and(|t| t.conn.is_some());
        self.next_conn_id += 1;
        self.parked.push(ConnState::new(self.next_conn_id));
        let ix = self.parked.len() - 1;
        self.swap_active(ix);
        if from_tab {
            self.show_home = true;
            self.conn.open_tab_on_connect = true;
        }
    }

    /// Switcher pick: activate `id` and show something that runs on it.
    /// A tab that is not bound yet (or lost its connection) is bound to
    /// `id`; a tab of another open connection makes way for `id`'s most
    /// recent tab, or a new query tab.
    pub(super) fn switch_conn(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_conn(id, cx);
        if self.conn.id != id || self.show_home {
            cx.notify();
            return;
        }
        let orphaned = self.active_tab_orphaned();
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        match tab.conn {
            None => tab.conn = Some(id),
            Some(_) if orphaned => {
                tab.conn = Some(id);
                // Its grid came from the closed connection.
                tab.last_result = None;
            }
            Some(current) if current == id => {}
            Some(_) => match self.tabs.iter().rposition(|t| t.conn == Some(id)) {
                Some(ix) => self.activate_tab(ix, cx),
                None => self.add_tab(window, cx),
            },
        }
        cx.notify();
    }

    /// After the active connection was disconnected: forget it and switch
    /// to another open connection, when there is one. Its tabs stay, as
    /// orphans that refuse to run until a connection is picked for them.
    /// With no other connection the disconnected slot stays active, so
    /// the sidebar can offer to reconnect it.
    pub(super) fn after_disconnect(&mut self, cx: &mut Context<Self>) {
        let closed = self.conn.id;
        for tab in &mut self.tabs {
            if tab.conn == Some(closed) {
                tab.last_result = None;
                tab.elapsed_ms = None;
                tab.plan_nodes = None;
                tab.plan_lines.clear();
                tab.table_state.update(cx, |state, cx| {
                    state.delegate_mut().set_result(QueryResult::default());
                    state.refresh(cx);
                });
            }
        }
        if let Some(next) = self.parked.iter().rev().find(|c| c.is_open()).map(|c| c.id) {
            self.activate_conn(next, cx);
            // Stay on a tab of the connection now active, when there is one.
            if !self.show_home && self.active_tab_orphaned() {
                if let Some(ix) = self.tabs.iter().rposition(|t| t.conn == Some(next)) {
                    self.activate_tab(ix, cx);
                }
            }
        }
    }

    /// Short connection name for a tab, or `None` when it needs none
    /// (only one connection in play and the tab is not orphaned).
    pub(super) fn tab_conn_label(&self, tab: &QueryTab) -> Option<(String, bool)> {
        let id = tab.conn?;
        match self.conn_by_id(id).filter(|c| c.is_open()) {
            Some(conn) => (self.open_conn_count() > 1).then(|| (conn.label(), false)),
            None => (self.open_conn_count() > 0).then(|| ("closed".to_string(), true)),
        }
    }

    /// Profile indexes move when one is removed: keep every connection's
    /// link to its profile right.
    pub(super) fn shift_profile_ix(&mut self, removed: usize) {
        for conn in std::iter::once(&mut self.conn).chain(self.parked.iter_mut()) {
            if let Some(last) = &mut conn.last_connection {
                last.profile_ix = match last.profile_ix {
                    Some(i) if i == removed => None,
                    Some(i) if i > removed => Some(i - 1),
                    other => other,
                };
            }
        }
    }

    /// The toolbar connection pill: shows the active connection; its menu
    /// switches between open connections and opens new ones.
    pub(super) fn render_conn_pill(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (dot_color, _, conn_detail) = self.conn_status(theme);
        let others = self.open_conn_count() - usize::from(self.conn.is_open());
        // With several open, the short profile name keeps the pill from
        // squeezing Commit/Rollback off the toolbar.
        let pill_label = if self.conn.session.is_some() && others > 0 {
            self.conn.label()
        } else if self.conn.session.is_some() {
            conn_detail
        } else {
            "Not connected".to_string()
        };
        // (id, name, user@db, active, uncommitted)
        let mut entries: Vec<(u64, String, String, bool, bool)> = Vec::new();
        for conn in std::iter::once(&self.conn).chain(self.parked.iter()) {
            if !conn.is_open() {
                continue;
            }
            let detail = conn
                .session
                .as_ref()
                .map_or("connecting…".to_string(), |s| {
                    format!("{} @ {}", s.user, s.database)
                });
            entries.push((
                conn.id,
                conn.label(),
                detail,
                conn.id == self.conn.id,
                conn.uncommitted,
            ));
        }
        let connected = self.conn.session.is_some();
        let busy = self.conn.busy;
        let view = cx.entity();
        let mut pill = Button::new("conn-pill")
            .outline()
            .small()
            .flex_none()
            .max_w(px(240.))
            .dropdown_caret(true)
            .child(status_dot(dot_color))
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(FONT_LABEL))
                    .line_height(px(LEADING_LABEL))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(pill_label),
            );
        if others > 0 {
            // How many more connections are open behind this one.
            pill = pill.child(
                div()
                    .flex_none()
                    .px(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .bg(theme.secondary)
                    .text_size(px(FONT_CAPTION))
                    .line_height(px(LEADING_CAPTION))
                    .text_color(theme.muted_foreground)
                    .child(format!("+{others}")),
            );
        }
        pill.dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(px(260.));
            if !entries.is_empty() {
                menu = menu.label("Koneksi terbuka");
            }
            for (id, name, detail, active, uncommitted) in entries.clone() {
                let switch_view = view.clone();
                let mark = if uncommitted { " • belum commit" } else { "" };
                menu = menu.item(
                    PopupMenuItem::new(format!("{name}  ({detail}){mark}"))
                        .checked(active)
                        .on_click(move |_, window, cx: &mut App| {
                            switch_view.update(cx, |this, cx| this.switch_conn(id, window, cx));
                        }),
                );
            }
            if !entries.is_empty() {
                menu = menu.separator();
            }
            let new_view = view.clone();
            let panel_view = view.clone();
            let disconnect_view = view.clone();
            menu.item(
                PopupMenuItem::new("Koneksi baru…")
                    .icon(Icon::new(IconName::Plus))
                    .on_click(move |_, window, cx: &mut App| {
                        new_view.update(cx, |this, cx| this.open_new_connection(window, cx));
                    }),
            )
            .item(
                PopupMenuItem::new("Buka koneksi tersimpan…")
                    .icon(Icon::new(IconName::Database))
                    .on_click(move |_, _, cx: &mut App| {
                        panel_view.update(cx, |this, cx| {
                            this.show_home = true;
                            this.rail = RailPanel::Connections;
                            cx.notify();
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("Disconnect koneksi ini")
                    .icon(Icon::new(IconName::LogOut))
                    .disabled(!connected || busy)
                    .on_click(move |_, window, cx: &mut App| {
                        disconnect_view.update(cx, |this, cx| {
                            let id = this.conn.id;
                            this.request_disconnect(id, window, cx)
                        });
                    }),
            )
        })
    }
}

impl Workbench {
    /// Sidebar rows for the open connections that are not active, under
    /// the active one's card. Click a row to switch to it.
    pub(super) fn render_parked_conns(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap(px(SPACE_1)).mx(px(SPACE_3));
        for conn in self.parked.iter().filter(|c| c.is_open()) {
            let id = conn.id;
            let dot = if conn.session.is_some() {
                theme.success
            } else {
                theme.warning
            };
            list = list.child(
                div()
                    .id(format!("parked-conn-{id}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .mt(px(SPACE_1))
                    .px(px(SPACE_2))
                    .h(px(TREE_ROW_HEIGHT))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.switch_conn(id, window, cx)),
                    )
                    .child(status_dot(dot))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .text_color(theme.muted_foreground)
                            .child(conn.label()),
                    )
                    .when(conn.uncommitted, |this| {
                        this.child(caption_text("belum commit".to_string(), theme))
                    }),
            );
        }
        list
    }
}

impl Workbench {
    /// Close connection `id`, active or parked (Home tile Disconnect).
    pub(super) fn disconnect_conn(&mut self, id: u64, cx: &mut Context<Self>) {
        let back_to = (self.conn.id != id).then_some(self.conn.id);
        self.activate_conn(id, cx);
        if self.conn.id != id {
            return;
        }
        self.disconnect(cx);
        // Closing a parked connection must not move the user off the one
        // they were on.
        if let Some(prev) = back_to {
            self.activate_conn(prev, cx);
        }
    }
}

impl Workbench {
    /// Disconnect from the UI: close connection `id` together with its
    /// tabs. When SQL tabs hold text, a dialog first offers to save them
    /// (Save asks for a file per tab; Cancel at any point keeps the
    /// connection).
    pub(super) fn request_disconnect(
        &mut self,
        id: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scripts: Vec<(u64, String)> = self
            .tabs
            .iter()
            .filter(|t| t.conn == Some(id) && t.data.is_none())
            .filter(|t| !t.editor.read(cx).value().trim().is_empty())
            .map(|t| (t.id, t.title.clone()))
            .collect();
        if scripts.is_empty() {
            self.finish_disconnect(id, cx);
            return;
        }
        let label = self.conn_by_id(id).map_or_else(String::new, |c| c.label());
        self.disconnect_prompt = Some(DisconnectPrompt { id, label, scripts });
        cx.notify();
    }

    fn close_disconnect_prompt(&mut self, cx: &mut Context<Self>) {
        self.disconnect_prompt = None;
        cx.notify();
    }

    /// Dialog answer "Simpan…": save each script tab to a file the user
    /// picks, then disconnect. Cancelling a file dialog stops there and
    /// keeps the connection.
    fn save_scripts_then_disconnect(&mut self, cx: &mut Context<Self>) {
        let Some(DisconnectPrompt { id, scripts, .. }) = self.disconnect_prompt.take() else {
            return;
        };
        cx.notify();
        cx.spawn(async move |view, cx| {
            for (tab_id, _) in scripts {
                let request = view.update(cx, |this, cx| {
                    let tab = this.tabs.iter().find(|t| t.id == tab_id)?;
                    let text = tab.editor.read(cx).value().to_string();
                    let name = script_filename(&tab.title);
                    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    let prompt = cx.prompt_for_new_path(&dir, Some(&name));
                    Some((tab.title.clone(), text, dir.join(name), prompt))
                });
                let Ok(Some((title, text, fallback, prompt))) = request else {
                    continue;
                };
                let path = match prompt.await {
                    Ok(Ok(Some(path))) => path,
                    // Cancelled: keep the connection and its tabs.
                    Ok(Ok(None)) => return,
                    // No file dialog on this platform.
                    _ => fallback,
                };
                let written = std::fs::write(&path, text);
                let ok = written.is_ok();
                let _ = view.update(cx, |this, cx| {
                    match written {
                        Ok(()) => this
                            .push_message(format!("Script {title} tersimpan: {}", path.display())),
                        Err(err) => this.push_message(format!(
                            "Gagal menyimpan {title}: {err}. Disconnect dibatalkan."
                        )),
                    }
                    cx.notify();
                });
                if !ok {
                    return;
                }
            }
            let _ = view.update(cx, |this, cx| this.finish_disconnect(id, cx));
        })
        .detach();
    }

    /// Dialog answer "Jangan simpan": disconnect and drop the scripts.
    fn discard_scripts_then_disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.disconnect_prompt.take() {
            self.finish_disconnect(prompt.id, cx);
        }
    }

    pub(super) fn render_disconnect_prompt(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(prompt) = self.disconnect_prompt.as_ref() else {
            return div().into_any_element();
        };
        let names = prompt
            .scripts
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let text =
            format!("Tab {names} masih berisi script dan akan ditutup. Simpan dulu ke file?");
        // AlertDialog layout: title and message, then a footer band with
        // the actions side by side.
        let card = div()
            .id("disconnect-prompt")
            .flex()
            .flex_col()
            .w(px(440.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_disconnect_prompt(cx);
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
                            .text_size(px(FONT_HEADING_MD))
                            .line_height(px(LEADING_HEADING_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("Disconnect {}?", prompt.label)),
                    )
                    .child(
                        div()
                            .text_size(px(FONT_BODY))
                            .line_height(px(LEADING_BODY))
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
                        Button::new("disconnect-cancel")
                            .flex_1()
                            .label("Batal")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_disconnect_prompt(cx)),
                            ),
                    )
                    .child(
                        Button::new("disconnect-discard")
                            .flex_1()
                            .label("Jangan simpan")
                            .danger()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discard_scripts_then_disconnect(cx)
                            })),
                    )
                    .child(
                        Button::new("disconnect-save")
                            .flex_1()
                            .label("Simpan…")
                            .primary()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.save_scripts_then_disconnect(cx)),
                            ),
                    ),
            );
        div()
            .id("disconnect-prompt-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(160.))
            .bg(gpui_kit::hsla(0., 0., 0., 0.35))
            .occlude()
            .child(card)
            .into_any_element()
    }

    /// Close connection `id` (if still open) and every tab bound to it.
    fn finish_disconnect(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.conn_by_id(id).is_some_and(|c| c.is_open()) {
            self.disconnect_conn(id, cx);
        }
        self.close_tabs_where(|_, t, _| t.conn == Some(id), cx);
        self.follow_tab_conn(cx);
        cx.notify();
    }
}

/// File name offered when saving a tab's script: its title, `.sql`
/// added unless already there, path separators replaced.
fn script_filename(title: &str) -> String {
    let base: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if base.to_ascii_lowercase().ends_with(".sql") {
        base
    } else {
        format!("{base}.sql")
    }
}

#[cfg(test)]
mod tests {
    use super::script_filename;

    #[test]
    fn script_filename_adds_sql_once_and_strips_separators() {
        assert_eq!(script_filename("Query 2"), "Query 2.sql");
        assert_eq!(script_filename("EMPLOYEES.sql"), "EMPLOYEES.sql");
        assert_eq!(script_filename("HR/EMP:x"), "HR_EMP_x.sql");
    }
}
