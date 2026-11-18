use super::*;

impl Workbench {
    pub(super) fn conn_status(
        &self,
        theme: &gpui_kit::component::Theme,
    ) -> (Hsla, &'static str, String) {
        if self.conn.busy && matches!(self.conn.pending_op, Some(PendingOp::Connect)) {
            return (theme.warning, "Connecting…", String::new());
        }
        if let Some(session) = &self.conn.session {
            let mode = if self.conn.demo_mode {
                "Demo"
            } else {
                "Connected"
            };
            return (
                theme.success,
                mode,
                format!("{} @ {}", session.user, session.database),
            );
        }
        if self.conn.connect_error.is_some() {
            return (
                theme.danger,
                "Connection failed",
                "lihat Messages".to_string(),
            );
        }
        (theme.muted_foreground, "Disconnected", String::new())
    }

    pub(super) fn render_main(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // `min_w(0)` + clip: without them a wide result grid's min-content
        // width pushes the column past the window edge, hiding the
        // toolbar's right side and the plan panel.
        let mut main = div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .bg(theme.background);
        // The tab strip lives at the top of the main column only (right of
        // the sidebar), above the toolbar — as in the design. No spacer
        // above the rail/sidebar: the design shares the top row between
        // the first rail item, the sidebar header, and the tab strip.
        main = main.child(self.render_tab_bar(theme, cx));
        main = main.child(self.render_toolbar(theme, cx));
        // The split wrappers below take `min_h(0)`: as flex items they
        // otherwise grow to their content (e.g. a long Messages list),
        // pushing the bottom panel past the window instead of scrolling.
        if self.whats_new_active() {
            main = main.child(self.render_whats_new_page(theme, cx));
        } else if self.show_home {
            main = main.child(self.render_home(theme, cx));
        } else if self.active_tab().kind != TabKind::Query {
            // Toad-style object tabs: the data grid (or the script) fills
            // the work area, with info and errors in a strip below.
            let top = if self.active_tab().kind == TabKind::Data {
                self.render_data_grid(theme, cx).into_any_element()
            } else {
                self.render_editor(cx).into_any_element()
            };
            main = main.child(if self.info_collapsed {
                // Folded: the work area takes everything but the header.
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h(px(0.)).child(top))
                    .child(self.render_info_panel(theme, cx))
            } else {
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("object-split")
                        .child(resizable_panel().child(top))
                        .child(
                            resizable_panel()
                                .size(px(150.))
                                .size_range(px(80.)..px(400.))
                                .child(self.render_info_panel(theme, cx)),
                        ),
                )
            });
        } else {
            main = main.child(
                div().flex_1().min_h(px(0.)).child(
                    v_resizable("editor-split")
                        .child(resizable_panel().child(self.render_editor(cx)))
                        .child(
                            resizable_panel()
                                .size(px(320.))
                                .size_range(px(180.)..px(560.))
                                .child(self.render_bottom_split(theme, cx)),
                        ),
                ),
            );
        }
        main
    }

    fn render_toolbar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let no_session = self.conn.session.is_none();
        let on_home = self.show_home;
        let mut bar = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .gap(px(SPACE_2))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        let data_tab = !on_home && self.active_tab().kind == TabKind::Data;
        // The buttons scroll sideways in a narrow window instead of pushing
        // the connection pill past the edge.
        let actions = div()
            .id("toolbar-actions")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .flex_shrink(1.)
            .min_w(px(0.))
            .overflow_x_scroll()
            // Like Navicat's "New Query" / Toad's "Editor": always opens a
            // fresh SQL editor tab, from any tab including Home.
            .child(
                Button::new("new-sql")
                    .small()
                    .icon(IconName::SquarePen)
                    .disabled(no_session)
                    .tooltip("Buka tab SQL editor baru")
                    .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx))),
            )
            .child(div().w(px(1.)).h(px(20.)).bg(theme.border))
            .child(
                Button::new("run")
                    .small()
                    .icon(if data_tab {
                        IconName::RefreshCw
                    } else {
                        IconName::Play
                    })
                    .primary()
                    .loading(self.conn.busy)
                    .disabled(on_home || no_session)
                    .tooltip(if data_tab {
                        "Refresh data (Cmd/Ctrl+Enter atau F5)"
                    } else {
                        "Run (Cmd/Ctrl+Enter atau F5)"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            )
            .when(!data_tab, |bar| {
                bar.child(
                    Button::new("run-current")
                        .small()
                        .icon(IconName::ListEnd)
                        .disabled(on_home || no_session || self.conn.busy)
                        .tooltip("Jalankan seleksi, atau statement di posisi kursor (dipisah ;)")
                        .on_click(cx.listener(|this, _, _, cx| this.run_current(cx))),
                )
            })
            .when(!data_tab, |bar| {
                bar.child(
                    Button::new("explain")
                        .small()
                        .tooltip("Explain plan")
                        .icon(IconName::ChartColumn)
                        .disabled(on_home || no_session || self.conn.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.explain(cx))),
                )
            })
            .when(!data_tab, |bar| {
                bar.child(
                    Button::new("format")
                        .small()
                        .tooltip("Beautify SQL")
                        .icon(IconName::WandSparkles)
                        .disabled(on_home || no_session)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.format_editor(window, cx)),
                        ),
                )
            })
            .child(
                Button::new("stop")
                    .small()
                    .tooltip("Stop (respons operasi yang berjalan diabaikan)")
                    .icon(IconName::Square)
                    .disabled(no_session || !self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                Button::new("commit")
                    .small()
                    .tooltip("Commit")
                    .icon(Icon::new(IconName::Check).text_color(theme.success))
                    .ghost()
                    .disabled(no_session || self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
            )
            .child(
                Button::new("rollback")
                    .small()
                    .tooltip("Rollback")
                    .icon(Icon::new(IconName::Undo2).text_color(theme.danger))
                    .ghost()
                    .disabled(no_session || self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.rollback(cx))),
            );
        bar = bar.child(actions).child(div().flex_1().min_w(px(SPACE_2)));
        bar.child(self.render_conn_pill(theme, cx))
    }

    pub(super) fn render_status_bar(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        let (dot_color, conn_label, conn_detail) = self.conn_status(theme);
        let conn_text = if conn_detail.is_empty() {
            conn_label.to_string()
        } else {
            format!("{conn_label} · {conn_detail}")
        };
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(STATUS_BAR_HEIGHT))
            .gap(px(SPACE_4))
            .px(px(SPACE_3))
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border);
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .min_w(px(0.))
                .child(status_dot(dot_color))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(FONT_CAPTION))
                        .line_height(px(LEADING_CAPTION))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(conn_text),
                ),
        );
        if let Some(elapsed) = self.active_tab().elapsed_ms {
            bar = bar.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1))
                    .child(icon(IconName::Clock, 12., theme.muted_foreground))
                    .child(caption_text(format!("{elapsed} ms"), theme)),
            );
        }
        if let Some(result) = &self.active_tab().last_result {
            bar = bar.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1))
                    .child(icon(IconName::Table, 12., theme.muted_foreground))
                    .child(caption_text(format!("{} rows", result.row_count()), theme)),
            );
        }
        // Right side: activity while busy, last action while connected,
        // empty while disconnected — the connection state already shows
        // once on the left, and every status line is mirrored to Messages.
        bar.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_row()
                .justify_end()
                .child(
                    caption_text(
                        if self.conn.busy {
                            "Running…".to_string()
                        } else if self.conn.session.is_some() {
                            self.conn.status.clone()
                        } else {
                            String::new()
                        },
                        theme,
                    )
                    .min_w(px(0.))
                    .truncate(),
                ),
        )
    }
}
