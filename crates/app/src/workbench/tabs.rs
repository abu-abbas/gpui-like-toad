use super::*;

impl Workbench {
    pub(super) fn render_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Home stays pinned on the left and "+" on the right; the tabs in
        // between scroll sideways (mouse wheel or trackpad) when they no
        // longer fit. `flex_none` keeps the bar at its full height: as a
        // flex child it used to shrink when the content below was tall.
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink(1.)
            .min_w(px(0.))
            .h_full()
            .gap(px(SPACE_1))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        for (ix, tab) in self.tabs.iter().enumerate() {
            let active = !self.show_home && ix == self.active_tab;
            strip = strip.child(self.render_query_tab(ix, tab, active, theme, cx));
        }
        // Always the last tab, after every query/data tab.
        if self.show_whats_new_tab() {
            strip = strip.child(self.render_whats_new_tab(theme, cx));
        }
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(TAB_BAR_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(self.render_home_tab(theme, cx))
            .child(strip)
            .child(
                div()
                    .id("tab-add")
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
                    .w(px(28.))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx)))
                    .child(icon(IconName::Plus, 16., theme.muted_foreground)),
            )
    }

    fn render_home_tab(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.show_home && !self.whats_new_active();
        let mut tab = div()
            .id("tab-home")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .rounded(px(RADIUS_MD))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_home = true;
                this.leave_whats_new();
                cx.notify();
            }));
        if active {
            tab = tab
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border)
                .text_color(theme.tab_active_foreground);
        } else {
            tab = tab
                .text_color(theme.tab_foreground)
                .hover(|this| this.bg(theme.secondary));
        }
        tab.child(icon(
            IconName::House,
            14.,
            if active {
                theme.primary
            } else {
                theme.muted_foreground
            },
        ))
        .child(
            div()
                .text_size(px(FONT_LABEL))
                .line_height(px(LEADING_LABEL))
                .font_weight(FontWeight::MEDIUM)
                .child("Home"),
        )
    }

    fn render_query_tab(
        &self,
        ix: usize,
        tab: &QueryTab,
        active: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The label area and the close button are siblings, not nested
        // click targets, so closing a tab never also activates it.
        let mut container = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.))
            .rounded(px(RADIUS_MD));
        let view = cx.entity();
        let tab_count = self.tabs.len();
        if active {
            container = container
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border);
        } else {
            container = container.hover(|this| this.bg(theme.secondary));
        }
        let label_color = if active {
            theme.tab_active_foreground
        } else {
            theme.tab_foreground
        };
        container
            .context_menu(move |menu, _, _| {
                let close = view.clone();
                let others = view.clone();
                let right = view.clone();
                let saved = view.clone();
                let all = view.clone();
                menu.min_w(px(200.))
                    .item(
                        PopupMenuItem::new("Close").on_click(move |_, _, cx: &mut App| {
                            close.update(cx, |this, cx| this.close_tab(ix, cx));
                        }),
                    )
                    .item(
                        PopupMenuItem::new("Close Others")
                            .disabled(tab_count < 2)
                            .on_click(move |_, _, cx: &mut App| {
                                others.update(cx, |this, cx| {
                                    this.close_tabs_where(|i, _, _| i != ix, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Close to the Right")
                            .disabled(ix + 1 >= tab_count)
                            .on_click(move |_, _, cx: &mut App| {
                                right.update(cx, |this, cx| {
                                    this.close_tabs_where(|i, _, _| i > ix, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Close Saved").on_click(move |_, _, cx: &mut App| {
                            saved.update(cx, |this, cx| {
                                this.close_tabs_where(|_, t, cx| Self::tab_is_saved(t, cx), cx)
                            });
                        }),
                    )
                    .item(
                        PopupMenuItem::new("Close All").on_click(move |_, _, cx: &mut App| {
                            all.update(cx, |this, cx| this.close_tabs_where(|_, _, _| true, cx));
                        }),
                    )
            })
            .child(
                div()
                    .id(format!("query-tab-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .h(relative(1.0))
                    .gap(px(SPACE_1))
                    .pl(px(SPACE_2))
                    .pr(px(SPACE_1))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_tab(ix, cx)))
                    .child(icon(
                        match tab.kind {
                            TabKind::Query => IconName::Database,
                            TabKind::Data => IconName::Table,
                            TabKind::Script => IconName::FileCode,
                        },
                        14.,
                        if active {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(label_color)
                            .child(tab.title.clone()),
                    )
                    // Which connection the tab runs on, once more than one
                    // is open (or "closed" when its connection is gone).
                    .when_some(self.tab_conn_label(tab), |this, (label, closed)| {
                        this.child(
                            div()
                                .flex_none()
                                .max_w(px(96.))
                                .truncate()
                                .px(px(SPACE_1))
                                .rounded(px(RADIUS_SM))
                                .bg(if closed {
                                    theme.danger.opacity(0.15)
                                } else {
                                    theme.secondary
                                })
                                .text_size(px(FONT_CAPTION))
                                .line_height(px(LEADING_CAPTION))
                                .text_color(if closed {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                })
                                .child(label),
                        )
                    }),
            )
            .child(
                div()
                    .id(format!("query-tab-close-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .w(px(20.))
                    .mr(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .hover(|this| this.bg(theme.danger).text_color(theme.danger_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| this.close_tab(ix, cx)))
                    .child(icon(IconName::X, 12., theme.muted_foreground)),
            )
    }
}

impl Workbench {
    pub(super) fn active_tab(&self) -> &QueryTab {
        self.tabs.get(self.active_tab).unwrap_or(&self.scratch_tab)
    }

    pub(super) fn active_tab_mut(&mut self) -> &mut QueryTab {
        if self.active_tab < self.tabs.len() {
            &mut self.tabs[self.active_tab]
        } else {
            &mut self.scratch_tab
        }
    }

    pub(super) fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_tab_id += 1;
        let id = self.next_tab_id;
        let completion = self.completion.clone();
        let mut tab = QueryTab::new(id, format!("Query {id}"), "", &completion, window, cx);
        tab.conn = self.conn.is_open().then_some(self.conn.id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_home = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(self.active_tab);
        cx.notify();
    }

    /// Every tab can be closed; only Home stays. Closing the last one
    /// goes back to Home.
    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.show_home = true;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if ix < self.active_tab {
            self.active_tab -= 1;
        }
        // The tab now showing may run on another connection.
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Close every tab for which `close(ix, tab)` is true. The active tab
    /// stays active when it survives; otherwise the nearest survivor is
    /// shown (or Home when none is left).
    pub(super) fn close_tabs_where(
        &mut self,
        close: impl Fn(usize, &QueryTab, &App) -> bool,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let old_active = self.active_tab;
        let doomed: Vec<bool> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, t)| close(ix, t, cx))
            .collect();
        let mut ix = 0;
        self.tabs.retain(|_| {
            let keep = !doomed[ix];
            ix += 1;
            keep
        });
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.show_home = true;
        } else if let Some(pos) = self.tabs.iter().position(|t| Some(t.id) == active_id) {
            self.active_tab = pos;
        } else {
            self.active_tab = old_active.min(self.tabs.len() - 1);
        }
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Whether a tab holds nothing that would be lost: a data tab with
    /// no staged or uncommitted cell edits, or an empty SQL editor.
    fn tab_is_saved(tab: &QueryTab, cx: &App) -> bool {
        match &tab.data {
            Some(d) => d.edits.is_empty(),
            None => tab.editor.read(cx).value().trim().is_empty(),
        }
    }

    pub(super) fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.show_home = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(ix);
        self.follow_tab_conn(cx);
        cx.notify();
    }

    pub(super) fn new_tab_with_sql(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_tab(window, cx);
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
    }

    pub(super) fn load_sql_into_active_tab(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_home {
            self.new_tab_with_sql(sql, window, cx);
            return;
        }
        let editor = self.active_tab().editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
        cx.notify();
    }
}
