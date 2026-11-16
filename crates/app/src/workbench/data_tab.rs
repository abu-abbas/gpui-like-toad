//! Data tab state and its filter bar: rows of `column | operator | value`
//! above the grid, applied together with AND or OR. The conditions become
//! an Oracle `WHERE` with bind variables in [`ora_core::filter`]; this
//! module only holds the UI state and renders the bar.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::Selectable as _;
use ora_core::filter::select_sql;
use ora_core::{FilterCondition, FilterJoin, FilterOp, TableFilter};

use super::*;

/// One editable condition in the filter bar.
pub(super) struct FilterRow {
    id: u64,
    enabled: bool,
    column: String,
    op: FilterOp,
    input: Entity<InputState>,
    _enter: Subscription,
}

impl FilterRow {
    fn condition(&self, cx: &App) -> FilterCondition {
        FilterCondition::new(
            self.column.clone(),
            self.op,
            self.input.read(cx).value().to_string(),
        )
    }
}

/// What a data tab shows: one table or view, and the filter on it.
pub(super) struct DataTab {
    pub(super) owner: String,
    pub(super) name: String,
    /// Column names of the table, known after the first fetch.
    pub(super) columns: Vec<String>,
    rows: Vec<FilterRow>,
    join: FilterJoin,
    /// The filter the grid currently reflects (what Refresh re-runs).
    pub(super) applied: TableFilter,
    show_sql: bool,
    next_row_id: u64,
}

impl DataTab {
    pub(super) fn new(owner: &str, name: &str) -> Self {
        DataTab {
            owner: owner.to_string(),
            name: name.to_string(),
            columns: Vec::new(),
            rows: Vec::new(),
            join: FilterJoin::And,
            applied: TableFilter::default(),
            show_sql: false,
            next_row_id: 0,
        }
    }

    /// Adopt the fetched column names; rows without a column yet (the
    /// initial empty row) default to the first one.
    pub(super) fn set_columns(&mut self, columns: Vec<String>) {
        if columns.is_empty() {
            return;
        }
        for row in &mut self.rows {
            if row.column.is_empty() {
                row.column = columns[0].clone();
            }
        }
        self.columns = columns;
    }

    /// The filter Apply All would run: every checked row.
    fn draft(&self, cx: &App) -> TableFilter {
        TableFilter::new(
            self.rows
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.condition(cx))
                .collect(),
            self.join,
        )
    }
}

impl Workbench {
    fn data_mut(&mut self) -> Option<&mut DataTab> {
        self.tabs.get_mut(self.active_tab)?.data.as_mut()
    }

    /// Insert a filter row after row `after` (or at the end).
    pub(super) fn add_filter_row(
        &mut self,
        after: Option<u64>,
        condition: Option<FilterCondition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Nilai"));
        // Enter in a value box applies all checked filters.
        let enter = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_filters(None, cx);
            }
        });
        let Some(data) = self.data_mut() else {
            return;
        };
        data.next_row_id += 1;
        let (column, op) = match &condition {
            Some(c) => (c.column.clone(), c.op),
            None => (
                data.columns.first().cloned().unwrap_or_default(),
                FilterOp::Eq,
            ),
        };
        let row = FilterRow {
            id: data.next_row_id,
            enabled: true,
            column,
            op,
            input: input.clone(),
            _enter: enter,
        };
        let at = after
            .and_then(|id| data.rows.iter().position(|r| r.id == id))
            .map_or(data.rows.len(), |ix| ix + 1);
        data.rows.insert(at, row);
        if let Some(c) = condition {
            Self::set_input(&input, c.value, window, cx);
        }
        cx.notify();
    }

    /// Remove a row; the bar always keeps one, so removing the last row
    /// swaps in a fresh empty one.
    fn remove_filter_row(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data_mut() else {
            return;
        };
        data.rows.retain(|r| r.id != id);
        if data.rows.is_empty() {
            self.add_filter_row(None, None, window, cx);
        }
        cx.notify();
    }

    fn update_filter_row(
        &mut self,
        id: u64,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut FilterRow),
    ) {
        if let Some(row) = self
            .data_mut()
            .and_then(|d| d.rows.iter_mut().find(|r| r.id == id))
        {
            f(row);
        }
        cx.notify();
    }

    /// Apply one row (`only`) or every checked row, then reload the grid.
    pub(super) fn apply_filters(&mut self, only: Option<u64>, cx: &mut Context<Self>) {
        if self.busy {
            self.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let filter = match only {
            Some(id) => TableFilter::new(
                data.rows
                    .iter()
                    .filter(|r| r.id == id)
                    .map(|r| r.condition(cx))
                    .collect(),
                data.join,
            ),
            None => data.draft(cx),
        };
        if let Some(data) = self.data_mut() {
            data.applied = filter;
        }
        self.reload_data(cx);
    }

    /// Drop the applied filter and reset the bar to one empty row.
    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(data) = self.data_mut() else {
            return;
        };
        data.rows.clear();
        data.applied = TableFilter::default();
        self.add_filter_row(None, None, window, cx);
        self.reload_data(cx);
    }

    /// Fetch the active data tab's table with its applied filter. The
    /// statement goes to the Info log with its bind values.
    pub(super) fn reload_data(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.session.is_none() {
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let (owner, name, filter) = (data.owner.clone(), data.name.clone(), data.applied.clone());
        let query = select_sql(&owner, &name, &filter, DEFAULT_MAX_ROWS);
        let mut log = query.sql.clone();
        if !query.binds.is_empty() {
            let binds: Vec<String> = query
                .binds
                .iter()
                .enumerate()
                .map(|(ix, v)| format!(":{} = '{v}'", ix + 1))
                .collect();
            log.push_str(&format!("   [{}]", binds.join(", ")));
        }
        self.push_message(log);
        self.status = format!("Memuat {owner}.{name}...");
        self.pending_sql = Some(query.sql);
        self.spawn_db(
            Job::QueryTable {
                owner,
                name,
                filter,
            },
            PendingOp::Run,
            cx,
        );
    }

    // ----- rendering -----

    pub(super) fn render_filter_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(data) = self.active_tab().data.as_ref() else {
            return div().into_any_element();
        };
        let applied: Vec<&FilterCondition> = data.applied.active().collect();
        let mut bar = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .px(px(SPACE_3))
            .py(px(SPACE_2))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border);
        for row in &data.rows {
            let condition = row.condition(cx);
            let is_applied = applied.contains(&&condition);
            bar = bar.child(self.render_filter_row(row, &data.columns, is_applied, theme, cx));
        }

        let join = data.join;
        let view = cx.entity();
        let join_button = Button::new("filter-join")
            .small()
            .child(select_trigger(format!("Gabung: {}", join.keyword()), theme))
            .tooltip("Cara menggabungkan filter yang dicentang")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(260.));
                for (option, label) in [
                    (FilterJoin::And, "AND: semua filter harus cocok"),
                    (FilterJoin::Or, "OR: salah satu filter cocok"),
                ] {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(option == join).on_click(
                        move |_, _, cx: &mut App| {
                            view.update(cx, |this, cx| {
                                if let Some(data) = this.data_mut() {
                                    data.join = option;
                                }
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let applied_count = applied.len();
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    Button::new("filter-sql")
                        .small()
                        .label("SQL")
                        .icon(IconName::FileCode)
                        .selected(data.show_sql)
                        .tooltip("Lihat query yang dihasilkan filter")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(data) = this.data_mut() {
                                data.show_sql = !data.show_sql;
                            }
                            cx.notify();
                        })),
                )
                .child(caption_text(
                    if applied_count == 0 {
                        "Tanpa filter. Enter di kolom nilai = Apply All.".to_string()
                    } else {
                        format!("{applied_count} filter aktif")
                    },
                    theme,
                ))
                .child(div().flex_1())
                .child(
                    Button::new("filter-clear")
                        .small()
                        .label("Clear")
                        .disabled(self.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.clear_filters(window, cx)),
                        ),
                )
                .child(join_button)
                .child(
                    Button::new("filter-apply-all")
                        .small()
                        .primary()
                        .label("Apply All")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.apply_filters(None, cx))),
                ),
        );
        if data.show_sql {
            let preview =
                select_sql(&data.owner, &data.name, &data.draft(cx), DEFAULT_MAX_ROWS).preview();
            bar = bar.child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .bg(theme.secondary)
                    .font_family(crate::MONO_FONT_FAMILY)
                    // No ligatures: `>=` must not render as one glyph.
                    .font_features(gpui_kit::FontFeatures::disable_ligatures())
                    .text_size(px(FONT_LABEL))
                    .line_height(px(LEADING_LABEL))
                    .children(preview.lines().map(|line| {
                        div()
                            .text_color(if line.starts_with("--") {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(line.to_string())
                    })),
            );
        }
        bar.into_any_element()
    }

    fn render_filter_row(
        &self,
        row: &FilterRow,
        columns: &[String],
        is_applied: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = row.id;
        let view = cx.entity();
        let check_view = view.clone();
        let checkbox = Checkbox::new(("filter-on", id))
            .checked(row.enabled)
            .on_click(move |checked, _, cx| {
                let checked = *checked;
                check_view.update(cx, |this, cx| {
                    this.update_filter_row(id, cx, |r| r.enabled = checked);
                });
            });

        let column_view = view.clone();
        let column_options = columns.to_vec();
        let current_column = row.column.clone();
        let column_button = Button::new(("filter-col", id))
            .small()
            .child(select_trigger(
                if row.column.is_empty() {
                    "(kolom)".to_string()
                } else {
                    row.column.clone()
                },
                theme,
            ))
            .w(px(180.))
            .disabled(columns.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(200.));
                for column in &column_options {
                    let pick = column.clone();
                    let view = column_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(column.clone())
                            .checked(*column == current_column)
                            .on_click(move |_, _, cx: &mut App| {
                                let pick = pick.clone();
                                view.update(cx, |this, cx| {
                                    this.update_filter_row(id, cx, |r| r.column = pick);
                                });
                            }),
                    );
                }
                menu
            });

        let op_view = view.clone();
        let current_op = row.op;
        let op_button = Button::new(("filter-op", id))
            .small()
            .child(select_trigger(row.op.label().to_string(), theme))
            .w(px(130.))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(160.));
                for op in FilterOp::ALL {
                    let view = op_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(op.label())
                            .checked(op == current_op)
                            .on_click(move |_, _, cx: &mut App| {
                                view.update(cx, |this, cx| {
                                    this.update_filter_row(id, cx, |r| r.op = op);
                                });
                            }),
                    );
                }
                menu
            });

        // The value box turns green while the grid reflects this row.
        let value = div()
            .flex_1()
            .min_w(px(0.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(if is_applied {
                theme.success
            } else {
                theme.input
            })
            .when(is_applied, |this| this.bg(theme.success.opacity(0.18)))
            .when(row.op.needs_value(), |this| {
                this.child(Input::new(&row.input).small().appearance(false).w_full())
            })
            .when(!row.op.needs_value(), |this| {
                this.h(px(26.))
                    .flex()
                    .items_center()
                    .px(px(SPACE_2))
                    .child(caption_text("(tidak perlu nilai)".to_string(), theme))
            });

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .child(checkbox)
            .child(column_button)
            .child(op_button)
            .child(value)
            .child(
                Button::new(("filter-apply", id))
                    .small()
                    .label(if is_applied { "Applied" } else { "Apply" })
                    .w(px(72.))
                    .disabled(self.busy)
                    .tooltip("Terapkan filter baris ini saja")
                    .on_click(cx.listener(move |this, _, _, cx| this.apply_filters(Some(id), cx))),
            )
            .child(
                Button::new(("filter-remove", id))
                    .small()
                    .icon(IconName::Minus)
                    .tooltip("Hapus baris filter")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remove_filter_row(id, window, cx)
                    })),
            )
            .child(
                Button::new(("filter-add", id))
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("Tambah baris filter")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_filter_row(Some(id), None, window, cx)
                    })),
            )
    }

    /// Footer of a data tab: row count, cap notice and timing.
    pub(super) fn render_data_footer(
        &self,
        theme: &gpui_kit::component::Theme,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let mut text = match &tab.last_result {
            Some(result) => {
                let rows = result.row_count();
                let mut text = format!("{rows} baris");
                if result.truncated {
                    text.push_str(&format!(" (dibatasi {DEFAULT_MAX_ROWS}, pakai filter)"));
                }
                text
            }
            None => String::new(),
        };
        if let Some(ms) = tab.elapsed_ms {
            text.push_str(&format!(" · {ms} ms"));
        }
        let filters = tab.data.as_ref().map_or(0, |d| d.applied.active().count());
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(PAGINATION_HEIGHT))
            .gap(px(SPACE_2))
            .px(px(SPACE_3))
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .when(filters > 0, |this| {
                this.child(icon(IconName::ListFilter, 14., theme.success))
                    .child(caption_text(format!("{filters} filter"), theme))
            })
            .child(div().flex_1())
            .child(caption_text(text, theme))
    }
}
