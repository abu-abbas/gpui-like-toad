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
        let pill_label = if self.conn.session.is_some() {
            conn_detail
        } else {
            "Not connected".to_string()
        };
        let others = self.open_conn_count() - usize::from(self.conn.is_open());
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
            .max_w(px(300.))
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
                    .on_click(move |_, _, cx: &mut App| {
                        disconnect_view.update(cx, |this, cx| this.disconnect(cx));
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
