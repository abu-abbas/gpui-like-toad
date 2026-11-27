use super::*;

impl Workbench {
    pub(super) fn render_title_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use gpui_kit::component::TitleBar;

        // The sidebar toggle lives next to Settings at the bottom of the
        // sidebar; the title bar only carries the window controls.
        let _ = (theme, cx);
        TitleBar::new()
    }

    pub(super) fn render_disconnected(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.whats_new_active() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(px(TAB_BAR_HEIGHT))
                        .px(px(SPACE_2))
                        .bg(theme.tab_bar)
                        .child(self.render_whats_new_tab(theme, cx)),
                )
                .child(self.render_whats_new_page(theme, cx))
                .into_any_element();
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(SPACE_2))
            .p(px(SPACE_4))
            .bg(theme.background)
            .child(icon(IconName::Plug, 38., theme::empty_hint(theme)))
            .child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(if self.conn.connection_lost {
                        "Connection lost"
                    } else {
                        "No active connection"
                    }),
            )
            .child(
                div()
                    .max_w(px(270.))
                    .text_center()
                    .text_color(theme::empty_hint(theme))
                    .child(if self.conn.connection_lost {
                        "Reconnect to resume browsing and running queries."
                    } else {
                        "Pick a connection on the left to browse schemas and open tables."
                    }),
            )
            .children(self.conn.connect_error.as_ref().map(|err| {
                div()
                    .max_w(px(360.))
                    .text_center()
                    .text_color(theme.danger)
                    .child(err.clone())
            }))
            .child(
                Button::new("empty-new-connection")
                    .outline()
                    .rounded(px(6.))
                    .h(px(30.))
                    .text_color(theme.muted_foreground)
                    .label("New connection")
                    .icon(IconName::Plus)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_new_connection(window, cx)),
                    ),
            )
            .when(self.conn.connection_lost, |view| {
                view.child(
                    Button::new("reconnect-lost")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label("Reconnect")
                        .disabled(self.conn.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reconnect_last(window, cx)),
                        ),
                )
            })
            .when(self.show_demo && self.sidebar_compact, |view| {
                view.child(
                    Button::new("empty-demo")
                        .small()
                        .ghost()
                        .icon(IconName::Play)
                        .label("Demo")
                        .disabled(self.conn.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.connect(true, window, cx)),
                        ),
                )
            })
            .into_any_element()
    }

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
            let detail = format!("{} @ {}", session.user, session.database);
            return (
                theme.success,
                mode,
                match &self.conn.extension_label {
                    Some(label) => format!("{detail} · {label}"),
                    None => detail,
                },
            );
        }
        if self.conn.connect_error.is_some() {
            return (
                theme.danger,
                "Connection failed",
                "see Messages".to_string(),
            );
        }
        if self.conn.connection_lost {
            return (theme.danger, "Connection lost", self.conn.label());
        }
        (
            theme.muted_foreground,
            "Disconnected",
            "No active connection".to_string(),
        )
    }

    /// Banner above a tab whose connection dropped: the tab and its SQL
    /// stay as they are, and one click logs in again with the saved login.
    fn render_lost_banner(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let cid = self.tabs.get(self.active_tab)?.conn?;
        let conn = self.conn_by_id(cid)?;
        if !conn.connection_lost {
            return None;
        }
        let name = conn.label();
        let detail = if conn.busy {
            "Reconnecting…".to_string()
        } else {
            conn.lost_reason
                .clone()
                .unwrap_or_else(|| "Connection lost.".to_string())
        };
        let busy = conn.busy;
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(SPACE_2))
                .px(px(SPACE_3))
                .py(px(SPACE_1))
                .bg(theme.warning.opacity(0.15))
                .border_b_1()
                .border_color(theme.border)
                .child(icon(IconName::Plug, 14., theme.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!(
                            "Disconnected from {name}. Your tabs and SQL are kept. {detail}"
                        )),
                )
                .child(
                    Button::new("lost-banner-reconnect")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label("Reconnect")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_conn(cid, cx);
                            this.reconnect_last(window, cx);
                        })),
                )
                .into_any_element(),
        )
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
        if !self.tabs.is_empty() || self.show_whats_new_tab() {
            main = main.child(self.render_tab_bar(theme, cx));
        }
        if !self.show_empty_workspace || self.conn.uncommitted {
            main = main.child(self.render_toolbar(theme, cx));
        }
        if !self.show_empty_workspace && !self.whats_new_active() {
            if let Some(banner) = self.render_lost_banner(theme, cx) {
                main = main.child(banner);
            }
        }
        // The split wrappers below take `min_h(0)`: as flex items they
        // otherwise grow to their content (e.g. a long Messages list),
        // pushing the bottom panel past the window instead of scrolling.
        if self.whats_new_active() {
            main = main.child(self.render_whats_new_page(theme, cx));
        } else if self.show_empty_workspace {
            main = main.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .bg(theme.background)
                    .child(icon(IconName::Table, 28., theme::empty_hint(theme)))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(theme::ui_size(theme, FONT_HEADING_MD))
                            .text_color(theme.muted_foreground)
                            .child("No objects open"),
                    )
                    .child(
                        div()
                            .text_center()
                            .text_color(theme::empty_hint(theme))
                            .child(
                                "Double-click an object in the explorer or open a new SQL editor.",
                            ),
                    ),
            );
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
        let no_active_tab = self.show_empty_workspace;
        let tab = self.active_tab();
        let is_data_tab = !no_active_tab && tab.kind == TabKind::Data;
        let busy = self.conn.busy;
        let stopped = self.conn.abandoned;
        let mut bar = div()
            .id("toolbar-actions")
            .flex()
            .flex_none()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .overflow_x_scroll()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        if !is_data_tab && !no_active_tab {
            bar = bar
                .child(self.render_conn_pill(theme, cx))
                .child(self.schema_picker("toolbar-schema", theme, cx));
        }
        if is_data_tab {
            let data = tab.data.as_ref().expect("data tab has table state");
            let data_view = data.view == DataView::Data;
            let object = ObjectRef {
                database: data.database.clone(),
                owner: data.owner.clone(),
                name: data.name.clone(),
                kind: data.kind.clone(),
                parent: None,
                detail: None,
            };
            let import_object = object.clone();
            let transfer_object = object.clone();
            let unavailable = busy || self.conn.session.is_none() || tab.conn != Some(self.conn.id);
            let action = |id: &'static str, symbol, color, label: &'static str| {
                Button::new(id)
                    .ghost()
                    .size(px(28.))
                    .icon(icon(symbol, 16., color))
                    .tooltip(label)
            };
            let divider = || {
                div()
                    .flex_none()
                    .mx(px(2.))
                    .w(px(1.))
                    .h(px(18.))
                    .bg(theme.border)
            };
            return bar
                .child(self.tx_button(
                    Button::new("table-commit").ghost().size(px(28.)),
                    true,
                    unavailable,
                    16.,
                    theme,
                    cx,
                ))
                .child(self.tx_button(
                    Button::new("table-rollback").ghost().size(px(28.)),
                    false,
                    unavailable,
                    16.,
                    theme,
                    cx,
                ))
                .child(divider())
                .child(
                    action(
                        "refresh-data",
                        IconName::RefreshCw,
                        theme.muted_foreground,
                        "Refresh data",
                    )
                    .disabled(unavailable)
                    .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                )
                .child(
                    action(
                        "toggle-data-filters",
                        IconName::ListFilter,
                        theme.muted_foreground,
                        "Table filters",
                    )
                    .disabled(!data_view)
                    .selected(data.show_filters)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(data) = this.data_mut() {
                            data.show_filters = !data.show_filters;
                            cx.notify();
                        }
                    })),
                )
                .child(
                    action(
                        "table-details",
                        IconName::PanelRight,
                        theme.muted_foreground,
                        "Row details",
                    )
                    .disabled(!data_view)
                    .selected(data.show_details)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(data) = this.data_mut() {
                            data.show_details = !data.show_details;
                            cx.notify();
                        }
                    })),
                )
                .child(divider())
                .child(
                    action(
                        "table-export",
                        IconName::Download,
                        theme.muted_foreground,
                        "Export table (CSV / XLSX)",
                    )
                    .disabled(unavailable)
                    .dropdown_menu({
                        let view = cx.entity();
                        move |menu, _, _| {
                            let csv_view = view.clone();
                            let safe_view = view.clone();
                            let xlsx_view = view.clone();
                            let csv_object = object.clone();
                            let safe_object = object.clone();
                            let xlsx_object = object.clone();
                            menu.item(PopupMenuItem::new("Export CSV").on_click(
                                move |_, _, cx: &mut App| {
                                    csv_view.update(cx, |this, cx| {
                                        this.export_table(csv_object.clone(), ExportFormat::Csv, cx)
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Export CSV (spreadsheet-safe)").on_click(
                                    move |_, _, cx: &mut App| {
                                        safe_view.update(cx, |this, cx| {
                                            this.export_table(
                                                safe_object.clone(),
                                                ExportFormat::CsvSpreadsheetSafe,
                                                cx,
                                            )
                                        });
                                    },
                                ),
                            )
                            .item(
                                PopupMenuItem::new("Export XLSX").on_click(
                                    move |_, _, cx: &mut App| {
                                        xlsx_view.update(cx, |this, cx| {
                                            this.export_table(
                                                xlsx_object.clone(),
                                                ExportFormat::Xlsx,
                                                cx,
                                            )
                                        });
                                    },
                                ),
                            )
                        }
                    }),
                )
                .child(
                    action(
                        "table-import",
                        IconName::Upload,
                        theme.muted_foreground,
                        "Import CSV into table",
                    )
                    .disabled(
                        unavailable
                            || import_object.kind != ObjectKind::Table
                            || self.active_kind().capabilities().databases,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.import_table(import_object.clone(), cx)
                    })),
                )
                .child(
                    action(
                        "table-transfer",
                        IconName::ArrowLeftRight,
                        theme.muted_foreground,
                        "Data Transfer…",
                    )
                    .disabled(
                        unavailable
                            || transfer_object.kind != ObjectKind::Table
                            || self.active_kind().capabilities().databases,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_data_transfer(Some(transfer_object.clone()), window, cx)
                    })),
                );
        } else if !no_active_tab {
            bar = bar
                .child(
                    Button::new("save-sql")
                        .small()
                        .ghost()
                        .icon(IconName::Save)
                        .tooltip("Save SQL")
                        .on_click(cx.listener(|this, _, _, cx| this.save_active_script(cx))),
                )
                .child(
                    Button::new("format")
                        .small()
                        .ghost()
                        .icon(IconName::WandSparkles)
                        .tooltip_with_action("Beautify SQL", &FormatSql, Some("Workbench"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.format_editor(window, cx)),
                        ),
                )
                .child(
                    Button::new("run")
                        .small()
                        .primary()
                        .icon(IconName::Play)
                        .loading(busy)
                        .tooltip_with_action(
                            "Run selection or current statement",
                            &RunCurrent,
                            Some("Workbench"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.run_current(cx))),
                )
                .when(self.dryrun_available(), |bar| {
                    bar.child(
                        Button::new("dryrun")
                            .small()
                            .ghost()
                            .icon(gpui_kit::assets::IconName::FlaskConical)
                            .label("Dry-run")
                            .disabled(busy)
                            .tooltip("Dry-run: run the statement without committing")
                            .on_click(cx.listener(|this, _, _, cx| this.open_dryrun(cx))),
                    )
                });
        }

        bar = bar.child(
            Button::new("stop")
                .small()
                .ghost()
                .icon(IconName::Square)
                .disabled(!busy || stopped)
                .tooltip("Stop waiting (the operation keeps running; busy until it finishes)")
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
        );
        if !no_active_tab && !is_data_tab {
            bar = bar
                .child(
                    Button::new("explain")
                        .small()
                        .ghost()
                        .icon(IconName::ChartColumn)
                        .disabled(busy)
                        .tooltip("Explain plan")
                        .on_click(cx.listener(|this, _, _, cx| this.explain(cx))),
                )
                .children(
                    self.active_dialect()
                        .capabilities()
                        .explain_analyze
                        .then(|| {
                            Button::new("explain-analyze")
                                .small()
                                .ghost()
                                .icon(IconName::Activity)
                                .disabled(busy)
                                .tooltip("Explain analyze (runs the statement, then rolls it back)")
                                .on_click(cx.listener(|this, _, _, cx| this.explain_analyze(cx)))
                        }),
                )
                .child(
                    Button::new("run-current")
                        .small()
                        .ghost()
                        .icon(IconName::ListEnd)
                        .disabled(busy)
                        .tooltip_with_action(
                            "Run as script (all statements, or the selection)",
                            &RunQuery,
                            Some("Workbench"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                );
        }
        bar.child(
            div()
                .w(px(1.))
                .h(px(18.))
                .mx(px(SPACE_1))
                .flex_none()
                .bg(theme.border),
        )
        .child(self.tx_button(
            Button::new("commit").small().ghost(),
            true,
            busy,
            14.,
            theme,
            cx,
        ))
        .child(self.tx_button(
            Button::new("rollback").small().ghost(),
            false,
            busy,
            14.,
            theme,
            cx,
        ))
    }

    /// Toolbar Commit (`commit`) or Rollback. In colour with a yellow dot
    /// while the connection holds uncommitted changes; dimmed otherwise
    /// but still clickable, since tracking is best effort (a procedure
    /// call that writes is not always recognised).
    fn tx_button(
        &self,
        button: Button,
        commit: bool,
        disabled: bool,
        size: f32,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let changes = self.conn.pending_changes();
        let open = changes > 0 && self.conn.session.is_some();
        let (symbol, color, verb) = if commit {
            (IconName::Check, theme.success, "Commit")
        } else {
            (IconName::Undo2, theme.danger, "Rollback")
        };
        let tip = if open {
            let noun = if changes == 1 { "change" } else { "changes" };
            format!("{verb} ({changes} uncommitted {noun})")
        } else {
            format!("{verb} (no uncommitted changes)")
        };
        let color = if open {
            color
        } else {
            theme.muted_foreground.opacity(0.6)
        };
        div()
            .relative()
            .flex_none()
            .child(
                button
                    .icon(icon(symbol, size, color))
                    .disabled(disabled)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if commit {
                            this.commit(cx)
                        } else {
                            this.rollback(cx)
                        }
                    })),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.warning),
                )
            })
    }

    pub(super) fn render_status_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (dot_color, conn_label, conn_detail) = self.conn_status(theme);
        // Connected: say which connection this is; the login, default
        // database and extension go in the tooltip, because the explorer
        // dropdown and the tab title already say which database is shown.
        let (conn_text, conn_tip) = match &self.conn.session {
            Some(session) if !self.conn.busy => {
                let mut tip = format!(
                    "Login: {}\nDefault database: {}",
                    session.user, session.database
                );
                if let Some(label) = &self.conn.extension_label {
                    tip.push_str(&format!("\n{label}"));
                }
                (format!("{conn_label} · {}", self.conn.label()), Some(tip))
            }
            _ if conn_detail.is_empty() => (conn_label.to_string(), None),
            _ => (format!("{conn_label} · {conn_detail}"), None),
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
                        .id("status-connection")
                        .min_w(px(0.))
                        .truncate()
                        .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                        .line_height(crate::theme::ui_size(theme, LEADING_CAPTION))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .when_some(conn_tip, |this, tip| {
                            this.tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tip.clone())
                                    .build(window, cx)
                            })
                        })
                        .child(conn_text),
                )
                .children(self.render_uncommitted_badge(theme, cx)),
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
        // The status line can also report file operations while disconnected.
        bar.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(SPACE_2))
                .children(self.render_sync_chip(theme, cx))
                // A spinning loader while a database job runs, so a long
                // query or refresh is visible at a glance.
                .when(self.conn.busy, |row| {
                    row.child(
                        gpui_kit::component::spinner::Spinner::new()
                            .xsmall()
                            .color(theme.primary),
                    )
                })
                .child(
                    caption_text(
                        if self.conn.busy {
                            "Running…".to_string()
                        } else if self.conn.session.is_some()
                            || self.conn.status != "Not connected."
                        {
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
