use super::*;

fn result_tab_label(
    label: impl Into<gpui_kit::SharedString>,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    div()
        .text_size(theme::ui_size(theme, FONT_LABEL))
        .line_height(theme::ui_size(theme, LEADING_LABEL))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label.into())
}

impl Workbench {
    /// Remember which panels are minimized for the next launch.
    pub(super) fn save_panel_layout(&self) {
        let layout = ora_core::store::PanelLayout {
            info_collapsed: self.info_collapsed,
            plan_collapsed: self.plan_collapsed,
            sidebar_width: Some(self.sidebar_width.round() as u32),
            explorer_width: Some(self.explorer_width.round() as u32),
        };
        if let Err(err) = ora_core::store::save_panel_layout(layout) {
            eprintln!("could not save panel layout: {err}");
        }
    }

    /// Spinner filling the area of a data tab view while that view's
    /// dictionary query runs; `None` once it has finished.
    fn view_loader(
        &self,
        op: PendingOp,
        theme: &gpui_kit::component::Theme,
    ) -> Option<gpui_kit::AnyElement> {
        let running = self.conn.busy
            && matches!(
                (&self.conn.pending_op, &op),
                (Some(PendingOp::DataStructure), PendingOp::DataStructure)
                    | (Some(PendingOp::DataIndexes), PendingOp::DataIndexes)
                    | (Some(PendingOp::DataDdl), PendingOp::DataDdl)
            );
        running.then(|| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(loading_state("Loading…", theme))
                .into_any_element()
        })
    }

    /// Full-area grid of a data tab, with pagination under it.
    pub(super) fn render_data_grid(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        if let Some(data) = &tab.data {
            let grid = tab.table_state.read(cx).delegate();
            if !std::rc::Rc::ptr_eq(&grid.insert_rows, &data.insert_rows)
                || (!data.insert_rows.is_empty() && grid.insert_disabled != self.conn.busy)
            {
                tab.table_state.update(cx, |state, cx| {
                    let grid = state.delegate_mut();
                    grid.insert_rows = data.insert_rows.clone();
                    grid.insert_disabled = self.conn.busy;
                    cx.notify();
                });
            }
        }
        let grid = if tab.last_result.is_some() {
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .key_context("OraDataGrid")
                // Enter / F2 edit the picked cell in place.
                .on_key_down(
                    cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        let plain = !event.keystroke.modifiers.modified();
                        if plain
                            && matches!(key, "enter" | "f2")
                            && this.edit_picked_cell(window, cx)
                        {
                            cx.stop_propagation();
                        }
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &gpui_kit::component::input::Copy, _, cx| {
                        let state = this.active_tab().table_state.read(cx);
                        let grid = state.delegate();
                        // Picked cells copy just those cells, as in
                        // Navicat; rows picked on the gutter copy rows.
                        let text = grid
                            .picked_cells_text()
                            .or_else(|| grid.selected_tsv(state.selected_row()));
                        if let Some(text) = text {
                            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                        }
                    }),
                )
                .capture_action(cx.listener(
                    |this, _: &gpui_kit::component::input::Paste, window, cx| {
                        let anchor = this.focused_insert_cell(window, cx);
                        if anchor.is_some()
                            && cx
                                .read_from_clipboard()
                                .and_then(|item| item.text())
                                .is_some_and(|s| s.contains(['\t', '\n', '\r']))
                        {
                            this.paste_grid_rows(anchor, window, cx);
                            cx.stop_propagation();
                        }
                    },
                ))
                .on_action(cx.listener(
                    |this, _: &gpui_kit::component::input::Paste, window, cx| {
                        if this.focused_insert_cell(window, cx).is_some() {
                            return;
                        }
                        // Picked cells take the paste as edits; otherwise
                        // the clipboard becomes new rows.
                        let picked = this
                            .active_tab()
                            .table_state
                            .read(cx)
                            .delegate()
                            .has_cell_selection();
                        if picked
                            && this
                                .active_tab()
                                .data
                                .as_ref()
                                .is_some_and(|d| d.kind == ObjectKind::Table)
                        {
                            this.paste_into_cells(cx);
                        } else {
                            this.paste_grid_rows(None, window, cx);
                        }
                    },
                ))
                .child(results_grid::with_scrollbar(&tab.table_state, theme, cx))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(if self.conn.busy {
                    loading_state("Loading data…", theme)
                } else {
                    empty_state_text("No data. See the Info panel for errors.".to_string(), theme)
                })
                .into_any_element()
        };
        // No overlay over a page's early rows: they are the new page.
        let reloading = self.conn.busy
            && matches!(self.conn.pending_op, Some(PendingOp::Run))
            && tab.last_result.as_ref().is_some_and(|r| r.row_count() > 0)
            && tab.data.as_ref().is_none_or(|d| d.partial_rows == 0);
        let grid = if reloading {
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .child(grid)
                .child(loading_overlay(theme))
                .into_any_element()
        } else {
            grid
        };
        let grid = if tab.data.as_ref().is_some_and(|d| d.show_details) {
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
                .overflow_hidden()
                .child(grid)
                .child(self.render_details_panel(theme, cx))
                .into_any_element()
        } else {
            grid
        };
        let view = tab.data.as_ref().map_or(DataView::Data, |d| d.view);
        let body = match (view, tab.data.as_ref()) {
            (DataView::Structure, Some(data)) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .child(
                    self.view_loader(PendingOp::DataStructure, theme)
                        .unwrap_or_else(|| {
                            div()
                                .flex_1()
                                .child(results_grid::with_scrollbar(&data.structure, theme, cx))
                                .into_any_element()
                        }),
                )
                .into_any_element(),
            (DataView::Index, Some(data)) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .child(
                    self.view_loader(PendingOp::DataIndexes, theme)
                        .unwrap_or_else(|| {
                            div()
                                .flex_1()
                                .child(results_grid::with_scrollbar(&data.indexes, theme, cx))
                                .into_any_element()
                        }),
                )
                .into_any_element(),
            (DataView::Ddl, Some(data)) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .when(self.active_kind() == DbKind::Oracle, |body| {
                    body.child(
                        div()
                            .flex()
                            .flex_none()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(SPACE_3))
                            .px_2()
                            .py_1()
                            .child(
                                Checkbox::new("ddl-show-storage")
                                    .small()
                                    .label("Show storage")
                                    .checked(data.show_storage)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.set_show_storage(*checked, cx);
                                    })),
                            )
                            .child(caption_text(
                                if data.show_storage {
                                    "Full script from DBMS_METADATA (slow the first time on a big database)."
                                } else {
                                    "Quick script from the data dictionary. Tick Show storage for the full DBMS_METADATA script."
                                }
                                .to_string(),
                                theme,
                            )),
                    )
                })
                .child(
                    self.view_loader(PendingOp::DataDdl, theme)
                        .unwrap_or_else(|| {
                            query::sql_editor(&data.ddl)
                                .readonly(true)
                                .into_any_element()
                        }),
                )
                .into_any_element(),
            _ => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .when(
                    tab.data.as_ref().is_some_and(|data| data.show_filters),
                    |body| body.child(self.render_filter_bar(theme, cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(grid),
                )
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .child(body)
            .child(self.render_data_footer(theme, cx))
    }

    /// Info strip under a data/script tab: what the tab shows, then the
    /// latest messages with errors highlighted.
    pub(super) fn render_info_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.info_collapsed;
        let tab = self.active_tab();
        let mut summary = tab.title.clone();
        if let Some(result) = &tab.last_result {
            summary.push_str(&format!(
                " · {} columns · {} rows",
                result
                    .columns
                    .iter()
                    .filter(|c| c.name != ora_core::ROWID_COLUMN)
                    .count(),
                result.row_count()
            ));
            let note = limit_note(result);
            if result.truncated && tab.data.is_some() && !result.byte_limited {
                summary.push_str(" (more rows available)");
            } else if result.truncated && !result.byte_limited {
                summary.push_str(&format!(" (limited to {DEFAULT_MAX_ROWS} rows)"));
            }
            summary.push_str(&note);
        }
        if let Some(ms) = tab.elapsed_ms {
            summary.push_str(&format!(" · {ms} ms"));
        }
        let start = self.messages.len().saturating_sub(MAX_MESSAGES_SHOWN);
        div()
            .flex()
            .flex_col()
            .size_full()
            .when(collapsed, |this| {
                this.h(px(SUB_TAB_HEIGHT + 1.)).flex_none()
            })
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(SUB_TAB_HEIGHT))
                    .px(px(SPACE_3))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(IconName::Info, 14., theme.muted_foreground))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Info"),
                    )
                    .child(caption_text(summary, theme))
                    .child(div().flex_1())
                    .child(
                        Button::new("info-clear")
                            .small()
                            .ghost()
                            .icon(IconName::Trash)
                            .tooltip("Clear messages")
                            .disabled(self.messages.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.clear_messages(cx))),
                    )
                    .child(
                        Button::new("info-toggle")
                            .small()
                            .ghost()
                            .icon(if collapsed {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .tooltip(if collapsed {
                                "Open Info panel"
                            } else {
                                "Minimize panel Info"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.info_collapsed = !this.info_collapsed;
                                this.save_panel_layout();
                                cx.notify();
                            })),
                    ),
            )
            .when(!collapsed, |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .id("info-messages")
                        .overflow_y_scroll()
                        .px(px(SPACE_3))
                        .py(px(SPACE_2))
                        .child(div().flex().flex_col().gap(px(SPACE_1)).children(
                            self.messages[start..].iter().rev().map(|m| {
                                div()
                                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                    .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                                    .text_color(if crate::error_log::is_error_message(m) {
                                        theme.danger
                                    } else {
                                        theme.foreground
                                    })
                                    .child(m.clone())
                            }),
                        )),
                )
            })
    }

    pub(super) fn render_bottom_split(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.plan_collapsed {
            return div()
                .flex()
                .size_full()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(self.render_results_column(theme, cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(34.))
                        .border_l_1()
                        .border_color(theme.border)
                        .child(self.plan_toggle(theme, cx)),
                )
                .into_any_element();
        }
        h_resizable("bottom-split")
            .child(resizable_panel().child(self.render_results_column(theme, cx)))
            .child(
                resizable_panel()
                    .size(px(PLAN_PANEL_WIDTH))
                    .size_range(px(240.)..px(560.))
                    .child(self.render_plan_panel(theme, cx)),
            )
            .into_any_element()
    }

    fn render_results_column(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .overflow_hidden()
            .bg(theme.background);
        let tab = self.active_tab();
        if tab.statement_results.len() > 1 {
            let mut strip = div()
                .id("batch-results")
                .flex()
                .items_center()
                .overflow_x_scroll()
                .flex_none()
                .gap(px(4.))
                .p(px(4.));
            for (label, dest) in [
                ("Messages", ResultsTab::Messages),
                ("Summary", ResultsTab::Summary),
            ] {
                strip = strip.child(
                    Button::new(label)
                        .flex_none()
                        .small()
                        .ghost()
                        .accessibility_label(label)
                        .child(result_tab_label(label, theme))
                        .selected(tab.results_tab == dest)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_tab_mut().results_tab = dest;
                            cx.notify();
                        })),
                );
            }
            for ix in 0..tab.statement_results.len() {
                let label = format!("Result {}", ix + 1);
                strip = strip.child(
                    Button::new(("statement-result", ix))
                        .flex_none()
                        .small()
                        .ghost()
                        .accessibility_label(label.clone())
                        .child(result_tab_label(label, theme))
                        .selected(
                            ix == tab.result_ix
                                && matches!(
                                    tab.results_tab,
                                    ResultsTab::Results | ResultsTab::Info
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_statement_result(this.active_tab, ix, cx)
                        })),
                );
            }
            col = col.child(strip);
        }
        if tab.statement_results.len() <= 1
            || !matches!(tab.results_tab, ResultsTab::Summary | ResultsTab::Messages)
        {
            col = col.child(self.render_sub_tab_bar(theme, cx));
        }
        // The content wrapper must be a flex container: the results content
        // is a `flex_1` div, and as a plain block child its flex sizing is
        // ignored, leaving it content-height. `DataTable` sizes itself with
        // `size_full`, which resolves against that indefinite height as
        // auto, so the virtualized row list collapses to 0px and no rows
        // paint even though the delegate holds data (only the header, with
        // its explicit height, showed). As a flex item the content gets a
        // definite flex-computed height and the rows render.
        col = col.child(
            // `min_h(0)`: without it the wrapper grows to fit a long
            // Messages list instead of letting it scroll.
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .child(self.render_results_content(theme, cx)),
        );
        if self.active_tab().results_tab == ResultsTab::Results {
            col = col.child(self.render_pagination(theme, cx));
        }
        col
    }

    pub(super) fn show_statement_result(
        &mut self,
        tab_ix: usize,
        result_ix: usize,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[tab_ix];
        let Some(entry) = tab.statement_results.get(result_ix) else {
            return;
        };
        tab.result_ix = result_ix;
        tab.last_result = entry.outcome.as_ref().ok().cloned();
        tab.elapsed_ms = tab.last_result.as_ref().map(|r| r.elapsed.as_millis());
        tab.results_tab = if tab.last_result.is_some() {
            ResultsTab::Results
        } else {
            ResultsTab::Info
        };
        tab.table_state.update(cx, |state, cx| {
            state
                .delegate_mut()
                .set_result(tab.last_result.clone().unwrap_or_default());
            state.refresh(cx);
        });
        cx.notify();
    }

    fn render_sub_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let rows = tab.last_result.as_ref().map(|r| r.row_count()).unwrap_or(0);
        let current = tab.results_tab;
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(SUB_TAB_HEIGHT))
            .px(px(SPACE_2))
            .gap(px(SPACE_1))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        let mut sections = vec![
            (ResultsTab::Results, format!("Data ({rows})")),
            (ResultsTab::Plan, "Explain Plan".to_string()),
            (
                ResultsTab::Messages,
                format!("Messages ({})", self.messages.len()),
            ),
        ];
        if self.dryrun_tab() == Some(tab.id) {
            sections.push((ResultsTab::DryRun, "Dry-run".to_string()));
        }
        if !tab.statement_results.is_empty() {
            sections.insert(1, (ResultsTab::Info, "Info".into()));
            if tab.statement_results.len() > 1 {
                sections.retain(|(which, _)| *which != ResultsTab::Messages);
            }
        }
        for (which, label) in sections {
            let active = current == which;
            let mut item = div()
                .id(format!("subtab-{label}"))
                .flex()
                .flex_row()
                .items_center()
                .h(relative(1.0))
                .px(px(SPACE_2))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active_tab_mut().results_tab = which;
                    cx.notify();
                }));
            if active {
                item = item
                    .border_b_2()
                    .border_color(theme.primary)
                    .text_color(theme.primary);
            } else {
                item = item
                    .text_color(theme.muted_foreground)
                    .hover(|this| this.text_color(theme.foreground));
            }
            bar = bar.child(item.child(result_tab_label(label, theme)));
        }
        bar = bar.child(div().flex_1());
        bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(self.render_export_dropdown(cx)),
        )
    }

    /// One Export dropdown (CSV / spreadsheet-safe CSV / XLSX) via the
    /// component `DropdownMenu` API; while an export runs it shows its
    /// progress and a Cancel button instead. The trigger shows no chevron
    /// glyph: `Button` renders none automatically and a unicode chevron is
    /// not allowed in this UI.
    fn render_export_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(export) = &self.result_export {
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    gpui_kit::component::spinner::Spinner::new()
                        .small()
                        .color(cx.theme().primary),
                )
                .child(caption_text(
                    format!("Exporting {}", export.progress_text()),
                    cx.theme(),
                ))
                .child(
                    Button::new("export-cancel")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip("Cancel export")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(export) = &this.result_export {
                                export.request_cancel();
                            }
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let has_result = self.active_tab().last_result.is_some();
        let view = cx.entity();
        Button::new("export")
            .ghost()
            .small()
            .child(result_tab_label("Export", cx.theme()))
            .icon(IconName::Download)
            .disabled(!has_result)
            .dropdown_menu(move |menu, _, _| {
                use super::result_export::ResultExportKind as Kind;
                let mut menu = menu;
                for (label, kind) in [
                    ("Export CSV", Kind::Csv),
                    ("Export CSV (spreadsheet-safe)", Kind::CsvSpreadsheetSafe),
                    ("Export XLSX", Kind::Xlsx),
                ] {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(label).disabled(!has_result).on_click(
                        move |_, _, cx: &mut App| {
                            view.update(cx, |this, cx| this.export_result(kind, cx));
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    fn render_results_content(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        match tab.results_tab {
            ResultsTab::Summary | ResultsTab::Info => {
                let mut panel = div()
                    .id("query-info")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(12.));
                for (ix, entry) in tab.statement_results.iter().enumerate() {
                    if tab.results_tab == ResultsTab::Info && ix != tab.result_ix {
                        continue;
                    }
                    let summary = match &entry.outcome {
                        Ok(r) => format!(
                            "{} rows{}  ·  {} columns  ·  {:.3} s total{}",
                            r.row_count(),
                            if r.truncated && !r.byte_limited {
                                " (limited)"
                            } else {
                                ""
                            },
                            r.columns.len(),
                            r.elapsed.as_secs_f64(),
                            limit_note(r)
                        ),
                        Err(err) => err.clone(),
                    };
                    let mut section = div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .pb(px(12.))
                        .child(format!("Result {}  ·  {}", ix + 1, summary))
                        .child(format!("{}  ·  {}", entry.started, tab.result_connection));
                    if let Ok(r) = &entry.outcome {
                        if let Some(fetch) = r.fetch_time {
                            section = section.child(format!(
                                "Query time: {:.3} s    Fetch time: {:.3} s",
                                r.elapsed.saturating_sub(fetch).as_secs_f64(),
                                fetch.as_secs_f64()
                            ));
                        }
                    }
                    panel = panel.child(section.child(entry.sql.clone()));
                }
                panel.into_any_element()
            }

            ResultsTab::Results => {
                if tab.last_result.is_none() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text(
                            if self.conn.session.is_none() {
                                "Connect first, then run a query to see results here."
                            } else if cfg!(target_os = "macos") {
                                "Run a query to see results here. ⌘↩ or F9 runs the current statement, F5 runs the whole script."
                            } else {
                                "Run a query to see results here. Ctrl+Enter or F9 runs the current statement, F5 runs the whole script."
                            }
                            .to_string(),
                            theme,
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .child(results_grid::with_scrollbar(&tab.table_state, theme, cx))
                        .into_any_element()
                }
            }
            ResultsTab::Plan => {
                if tab.plan_lines.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text(
                            "Run Explain to see the DBMS_XPLAN text output. The plan tree is in the right panel.".to_string(),
                            theme,
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .id("plan-text-scroll")
                        .overflow_y_scroll()
                        .p(px(SPACE_3))
                        .child(div().flex().flex_col().children(tab.plan_lines.iter().map(
                            |line| {
                                div()
                                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                    .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .child(line.clone())
                            },
                        )))
                        .into_any_element()
                }
            }
            ResultsTab::Messages if tab.statement_results.len() > 1 => div()
                .id("batch-messages")
                .flex_1()
                .overflow_y_scroll()
                .p(px(12.))
                .children(tab.statement_results.iter().enumerate().map(|(ix, entry)| {
                    match &entry.outcome {
                        Ok(r) => div().child(format!(
                            "Result {} · {} rows fetched{}{}",
                            ix + 1,
                            r.row_count(),
                            if r.truncated && !r.byte_limited {
                                " (limited)"
                            } else {
                                ""
                            },
                            limit_note(r)
                        )),
                        Err(e) => div().text_color(theme.danger).child(format!(
                            "Result {} · {}",
                            ix + 1,
                            e
                        )),
                    }
                }))
                .into_any_element(),
            ResultsTab::DryRun if self.dryrun_tab() == Some(tab.id) => {
                self.render_dryrun(theme, cx).into_any_element()
            }
            ResultsTab::Messages | ResultsTab::DryRun => {
                if self.messages.is_empty() {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(empty_state_text("No messages yet.".to_string(), theme))
                        .into_any_element()
                } else {
                    let start = self.messages.len().saturating_sub(MAX_MESSAGES_SHOWN);
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .id("messages-scroll")
                        .overflow_y_scroll()
                        .track_scroll(&self.messages_scroll)
                        .p(px(SPACE_3))
                        .child(div().flex().flex_col().gap(px(SPACE_1)).children(
                            self.messages[start..].iter().map(|m| {
                                div()
                                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                    .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .child(m.clone())
                            }),
                        ))
                        .into_any_element()
                }
            }
        }
    }

    fn render_pagination(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let (first, last, total, current, total_pages, page_size) = {
            let state = tab.table_state.read(cx);
            let delegate = state.delegate();
            let (first, last, total) = delegate.page_range();
            (
                first,
                last,
                total,
                delegate.page(),
                delegate.total_pages(),
                delegate.page_size(),
            )
        };
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(PAGINATION_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border);
        row = row.child(self.render_page_nav(
            "first",
            IconName::ChevronsLeft,
            0,
            current > 0,
            theme,
            cx,
        ));
        row = row.child(self.render_page_nav(
            "prev",
            IconName::ChevronLeft,
            current.saturating_sub(1),
            current > 0,
            theme,
            cx,
        ));
        for item in page_items(current, total_pages) {
            match item {
                PageItem::Gap => {
                    row = row.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .h(px(24.))
                            .w(px(24.))
                            .child(caption_text("…".to_string(), theme)),
                    );
                }
                PageItem::Page(page) => {
                    let active = page == current;
                    let mut button = div()
                        .id(format!("page-{page}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .h(px(24.))
                        .w(px(24.))
                        .rounded(px(RADIUS_SM))
                        .on_click(cx.listener(move |this, _, _, cx| this.goto_page(page, cx)));
                    if active {
                        button = button.bg(theme.primary);
                    } else {
                        button = button.hover(|this| this.bg(theme.secondary));
                    }
                    row = row.child(
                        button.child(
                            div()
                                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if active {
                                    theme.primary_foreground
                                } else {
                                    theme.foreground
                                })
                                .child(format!("{}", page + 1)),
                        ),
                    );
                }
            }
        }
        row = row.child(self.render_page_nav(
            "next",
            IconName::ChevronRight,
            current + 1,
            current + 1 < total_pages,
            theme,
            cx,
        ));
        row = row.child(self.render_page_nav(
            "last",
            IconName::ChevronsRight,
            total_pages.saturating_sub(1),
            current + 1 < total_pages,
            theme,
            cx,
        ));
        row.child(div().flex_1())
            .child(caption_text(format!("{first}–{last} of {total}"), theme))
            .child(
                div()
                    .pl(px(SPACE_4))
                    .child(caption_text(format!("{page_size} / page"), theme)),
            )
    }

    fn render_page_nav(
        &self,
        name: &str,
        icon_name: IconName,
        target: usize,
        enabled: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("page-nav-{name}"))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .h(px(24.))
            .w(px(24.))
            .rounded(px(RADIUS_SM))
            .hover(|this| this.bg(theme.secondary))
            .on_click(cx.listener(move |this, _, _, cx| {
                if enabled {
                    this.goto_page(target, cx);
                }
            }))
            .child(icon(
                icon_name,
                14.,
                if enabled {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
            ))
    }

    fn plan_toggle(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Button {
        let label = if self.plan_collapsed {
            "Expand Explain Plan"
        } else {
            "Minimize Explain Plan"
        };
        Button::new("toggle-plan-panel")
            .small()
            .ghost()
            .tooltip(label)
            .child(icon(
                if self.plan_collapsed {
                    IconName::ChevronLeft
                } else {
                    IconName::ChevronRight
                },
                14.,
                theme.muted_foreground,
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.plan_collapsed = !this.plan_collapsed;
                this.save_panel_layout();
                cx.notify();
            }))
    }

    fn render_plan_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let mut panel = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border);
        panel = panel
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(SUB_TAB_HEIGHT))
                    .px(px(SPACE_3))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                            .line_height(crate::theme::ui_size(theme, LEADING_CAPTION))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("EXPLAIN PLAN"),
                    )
                    .child(div().flex_1())
                    .child(self.plan_toggle(theme, cx)),
            )
            .child(div().px(px(SPACE_3)).pb(px(SPACE_2)).child(caption_text(
                "Estimated plan (EXPLAIN PLAN), not runtime statistics.".to_string(),
                theme,
            )));
        if let Some(nodes) = &tab.plan_nodes {
            let view = cx.entity();
            let expanded = tab.plan_expanded.clone();
            panel = panel.child(
                div()
                    .flex_1()
                    .id("plan-tree-scroll")
                    .overflow_y_scroll()
                    .p(px(SPACE_2))
                    .child(plan::render_plan_tree(nodes, &expanded, view, cx)),
            );
        } else if !tab.plan_lines.is_empty() {
            panel =
                panel.child(
                    div()
                        .flex_1()
                        .id("plan-text-scroll-panel")
                        .overflow_y_scroll()
                        .p(px(SPACE_3))
                        .child(div().flex().flex_col().children(tab.plan_lines.iter().map(
                            |line| {
                                div()
                                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                    .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .child(line.clone())
                            },
                        ))),
                );
        } else {
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .p(px(SPACE_4))
                    .child(empty_state_text(
                        "Run Explain to see the execution plan tree here.".to_string(),
                        theme,
                    )),
            );
        }
        panel
    }
}

/// One slot in the numbered pagination row: a clickable page or an
/// ellipsis gap between the pinned first/last pages and the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageItem {
    Page(usize),
    Gap,
}

/// Numbered pagination with pinned first/last pages and ellipsis gaps:
/// at most 5 window pages, centered on the current page when possible.
/// Pages are 0-based.
fn page_items(current: usize, total: usize) -> Vec<PageItem> {
    const WINDOW: usize = 5;
    if total == 0 {
        return Vec::new();
    }
    if total <= WINDOW + 2 {
        return (0..total).map(PageItem::Page).collect();
    }
    let start = current.saturating_sub(2).min(total - WINDOW);
    let end = start + WINDOW;
    let mut items = Vec::new();
    if start > 0 {
        items.push(PageItem::Page(0));
        if start > 1 {
            items.push(PageItem::Gap);
        }
    }
    for page in start..end {
        items.push(PageItem::Page(page));
    }
    if end < total {
        if end < total - 1 {
            items.push(PageItem::Gap);
        }
        items.push(PageItem::Page(total - 1));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_items_pin_first_last_with_ellipsis() {
        use PageItem::{Gap, Page};
        assert!(page_items(0, 0).is_empty());
        assert_eq!(page_items(0, 1), vec![Page(0)]);
        assert_eq!(page_items(0, 3), vec![Page(0), Page(1), Page(2)]);
        assert_eq!(
            page_items(0, 7),
            vec![
                Page(0),
                Page(1),
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Page(6)
            ]
        );
        assert_eq!(
            page_items(0, 9),
            vec![Page(0), Page(1), Page(2), Page(3), Page(4), Gap, Page(8)]
        );
        assert_eq!(
            page_items(4, 9),
            vec![
                Page(0),
                Gap,
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Page(6),
                Gap,
                Page(8)
            ]
        );
        assert_eq!(
            page_items(8, 9),
            vec![Page(0), Gap, Page(4), Page(5), Page(6), Page(7), Page(8)]
        );
        assert_eq!(page_items(99, 9), page_items(8, 9));
        assert_eq!(
            page_items(3, 8),
            vec![
                Page(0),
                Page(1),
                Page(2),
                Page(3),
                Page(4),
                Page(5),
                Gap,
                Page(7)
            ]
        );
    }
}

impl Workbench {
    /// Show a fresh result in the target tab. The tab and the grid share
    /// one `Arc`: no cell is copied.
    pub(super) fn set_grid_result_for(
        &mut self,
        conn_id: u64,
        result: QueryResult,
        cx: &mut Context<Self>,
    ) {
        let result = Arc::new(result);
        let tab = self.target_tab_for_mut(conn_id);
        tab.last_result = Some(result.clone());
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_result(result);
            state.refresh(cx);
        });
        self.enforce_result_budget(cx);
    }

    /// Like [`Workbench::set_grid_result_for`] for the rest of a page whose
    /// first rows the grid shows already ([`crate::worker::Response::QueryPartial`]).
    pub(super) fn grow_grid_result_for(
        &mut self,
        conn_id: u64,
        result: QueryResult,
        cx: &mut Context<Self>,
    ) {
        let result = Arc::new(result);
        let tab = self.target_tab_for_mut(conn_id);
        tab.last_result = Some(result.clone());
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().extend_result(result);
            state.refresh(cx);
        });
        self.enforce_result_budget(cx);
    }

    pub(crate) fn toggle_plan_node(&mut self, id: i64) {
        let expanded = &mut self.active_tab_mut().plan_expanded;
        if !expanded.insert(id) {
            expanded.remove(&id);
        }
    }

    fn goto_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        let tab_id = tab.id;
        tab.table_state.update(cx, |state, cx| {
            state.delegate_mut().set_page(page);
            state.refresh(cx);
        });
        self.clob_rows_changed(tab_id, None, cx);
        cx.notify();
    }
}

/// Bytes the results of all open tabs may hold together. A new result
/// that pushes the total over releases the biggest results of other tabs
/// (never the tab that just ran, never a data tab with changes).
pub(super) const MAX_OPEN_RESULT_BYTES: usize = 256 * 1024 * 1024;

/// Text an `outcome` gets when its result was released for memory.
pub(super) const RESULT_RELEASED: &str =
    "Result released to free memory (too many large results open). Run it again to see it.";

/// Why a result was cut, for status and Info lines: empty for a normal
/// result, else e.g. " (stopped at the 32 MiB result limit) · 3 cells clipped".
pub(super) fn limit_note(result: &QueryResult) -> String {
    let mut note = String::new();
    if result.byte_limited {
        note.push_str(&format!(
            " (stopped at the {} MiB result limit)",
            ora_core::fetch::MAX_RESULT_BYTES / (1024 * 1024)
        ));
    }
    let clipped = result.clipped_cells();
    if clipped > 0 {
        note.push_str(&format!(
            " · {clipped} long values show only their start (right click > Load full value)"
        ));
    }
    note
}

/// The distinct results a tab holds (its statements and the shown one),
/// with their sizes. Shared `Arc`s are counted once.
fn tab_result_bytes(tab: &QueryTab, seen: &mut HashSet<usize>, cx: &App) -> usize {
    let statements = tab
        .statement_results
        .iter()
        .filter_map(|entry| entry.outcome.as_ref().ok());
    statements
        .chain(tab.last_result.iter())
        .filter(|result| seen.insert(Arc::as_ptr(result) as usize))
        .map(|result| result.heap_bytes())
        .sum::<usize>()
        // Full values loaded into the grid live outside the shared result.
        + tab.table_state.read(cx).delegate().override_bytes()
}

/// Bytes still free under [`MAX_OPEN_RESULT_BYTES`] when open results hold
/// `used`.
pub(super) fn result_room(used: usize) -> usize {
    MAX_OPEN_RESULT_BYTES.saturating_sub(used)
}

impl Workbench {
    /// Bytes all open results hold: the distinct results of every tab plus
    /// the full values loaded into their grids.
    pub(super) fn open_result_bytes(&self, cx: &App) -> usize {
        let mut seen = HashSet::new();
        self.tabs
            .iter()
            .map(|tab| tab_result_bytes(tab, &mut seen, cx))
            .sum()
    }
}

/// Which tabs to release, biggest first, so that `sizes` (bytes per tab;
/// `None` = must be kept) fits in `budget`.
pub(super) fn tabs_to_release(sizes: &[Option<usize>], kept: usize, budget: usize) -> Vec<usize> {
    let mut total = kept + sizes.iter().flatten().sum::<usize>();
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&ix| sizes[ix].is_some()).collect();
    order.sort_by_key(|&ix| std::cmp::Reverse(sizes[ix]));
    let mut release = Vec::new();
    for ix in order {
        if total <= budget {
            break;
        }
        total -= sizes[ix].unwrap_or(0);
        release.push(ix);
    }
    release
}

impl Workbench {
    /// Keep all open results under [`MAX_OPEN_RESULT_BYTES`]: release the
    /// biggest results of other tabs until the total fits.
    pub(super) fn enforce_result_budget(&mut self, cx: &mut Context<Self>) {
        let keep = self.target_ix();
        let mut seen = HashSet::new();
        let mut kept = 0;
        let sizes: Vec<Option<usize>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let bytes = tab_result_bytes(tab, &mut seen, cx);
                let pinned = Some(ix) == keep || tab.data.as_ref().is_some_and(|d| d.has_staged());
                if pinned {
                    kept += bytes;
                    None
                } else {
                    Some(bytes).filter(|b| *b > 0)
                }
            })
            .collect();
        for ix in tabs_to_release(&sizes, kept, MAX_OPEN_RESULT_BYTES) {
            let tab = &mut self.tabs[ix];
            tab.last_result = None;
            for entry in &mut tab.statement_results {
                if entry.outcome.is_ok() {
                    entry.outcome = Err(RESULT_RELEASED.to_string());
                }
            }
            tab.table_state.update(cx, |state, cx| {
                state.delegate_mut().set_result(QueryResult::default());
                state.refresh(cx);
            });
            let title = tab.title.clone();
            self.push_message(format!("{title}: {RESULT_RELEASED}"));
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn releases_biggest_other_tabs_until_under_budget() {
        // Tab 1 is pinned (None); 100 kept in it.
        let sizes = [Some(50), None, Some(200), Some(10)];
        assert_eq!(tabs_to_release(&sizes, 100, 400), Vec::<usize>::new());
        assert_eq!(tabs_to_release(&sizes, 100, 300), vec![2]);
        assert_eq!(tabs_to_release(&sizes, 100, 120), vec![2, 0]);
        // Pinned results alone over budget: release everything else.
        assert_eq!(tabs_to_release(&sizes, 1000, 120), vec![2, 0, 3]);
    }

    #[test]
    fn room_is_what_the_open_results_leave_and_never_negative() {
        assert_eq!(result_room(0), MAX_OPEN_RESULT_BYTES);
        assert_eq!(result_room(MAX_OPEN_RESULT_BYTES - 10), 10);
        assert_eq!(result_room(MAX_OPEN_RESULT_BYTES + 5), 0);
    }
}
