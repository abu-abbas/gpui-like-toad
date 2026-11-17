//! Data tab state and its filter bar: rows of `column | operator | value`
//! above the grid, applied together with AND or OR. The conditions become
//! an Oracle `WHERE` with bind variables in [`ora_core::filter`]; this
//! module only holds the UI state and renders the bar.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::Selectable as _;
use ora_core::filter::select_sql;
use ora_core::{
    FilterCondition, FilterJoin, FilterOp, TableFilter, TablePage, MAX_PAGE_LIMIT, ROWID_COLUMN,
};

use super::cell_edit::CellEdit;
use crate::theme::{FONT_GRID, LEADING_GRID};

/// Rows shown at once in the Structure / Index grids (no paging there).
const INFO_GRID_ROWS: usize = 1000;

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

/// The bottom tabs of a data tab: one table, four views of it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DataView {
    Data,
    Structure,
    Index,
    Ddl,
}

impl DataView {
    const ALL: [DataView; 4] = [
        DataView::Data,
        DataView::Structure,
        DataView::Index,
        DataView::Ddl,
    ];

    fn label(self) -> &'static str {
        match self {
            DataView::Data => "Data",
            DataView::Structure => "Structure",
            DataView::Index => "Index",
            DataView::Ddl => "DDL",
        }
    }
}

/// What a data tab shows: one table or view, and the filter on it.
pub(super) struct DataTab {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) kind: ObjectKind,
    pub(super) view: DataView,
    /// Own grids/editor for the non-data views, so switching views never
    /// disturbs the data grid's rows, sort or scroll position.
    pub(super) structure: Entity<TableState<ResultsDelegate>>,
    pub(super) indexes: Entity<TableState<ResultsDelegate>>,
    pub(super) ddl: Entity<EditorState>,
    /// Column names of the table, known after the first fetch.
    pub(super) columns: Vec<String>,
    rows: Vec<FilterRow>,
    join: FilterJoin,
    /// The filter the grid currently reflects (what Refresh re-runs).
    pub(super) applied: TableFilter,
    show_sql: bool,
    /// The row Details panel on the right of the grid.
    pub(super) show_details: bool,
    /// Redraws the workbench when the grid's selected row changes.
    pub(super) _grid_observe: Option<Subscription>,
    next_row_id: u64,
    /// The page the grid currently reflects (what Refresh re-runs).
    pub(super) page: TablePage,
    limit_input: Entity<InputState>,
    offset_input: Entity<InputState>,
    /// Columns hidden by the Columns picker; survive refetches.
    pub(super) hidden: HashSet<String>,
    /// Cell edits staged or applied but not yet committed.
    pub(super) edits: Vec<CellEdit>,
    _page_subs: Vec<Subscription>,
}

impl DataTab {
    pub(super) fn new(
        owner: &str,
        name: &str,
        kind: ObjectKind,
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> Self {
        let mut info_grid = |cx: &mut Context<Workbench>| {
            cx.new(|cx| {
                TableState::new(
                    ResultsDelegate::empty().with_page_size(INFO_GRID_ROWS),
                    &mut *window,
                    cx,
                )
            })
        };
        let structure = info_grid(cx);
        let indexes = info_grid(cx);
        let ddl = cx.new(|cx| {
            EditorState::new(&mut *window, cx)
                .language("sql")
                .line_number(true)
                .folding(true)
        });
        // Tables also fetch each row's ROWID, which cell edits go by.
        let page = TablePage::default().with_rowid(kind == ObjectKind::Table);
        let limit_input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("Limit")
                .default_value(page.limit.to_string())
        });
        let offset_input = cx.new(|cx| {
            InputState::new(&mut *window, cx)
                .placeholder("Offset")
                .default_value(page.offset.to_string())
        });
        // Enter in either box jumps to that page.
        let page_subs = [&limit_input, &offset_input]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.go_page(None, window, cx);
                    }
                })
            })
            .collect();
        DataTab {
            owner: owner.to_string(),
            name: name.to_string(),
            kind,
            view: DataView::Data,
            structure,
            indexes,
            ddl,
            columns: Vec::new(),
            rows: Vec::new(),
            join: FilterJoin::And,
            applied: TableFilter::default(),
            show_sql: false,
            show_details: false,
            _grid_observe: None,
            next_row_id: 0,
            page,
            limit_input,
            offset_input,
            hidden: HashSet::new(),
            edits: Vec::new(),
            _page_subs: page_subs,
        }
    }

    /// Adopt the fetched column names; rows without a column yet (the
    /// initial empty row) default to the first one.
    pub(super) fn set_columns(&mut self, columns: Vec<String>) {
        let columns: Vec<String> = columns.into_iter().filter(|c| c != ROWID_COLUMN).collect();
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
    pub(super) fn data_mut(&mut self) -> Option<&mut DataTab> {
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
        let enter = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_filters(None, window, cx);
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

    /// Quick Filter from a cell's context menu: add the condition as a
    /// new row (replacing a lone, still-empty row) and apply every
    /// checked row.
    pub(super) fn quick_filter(
        &mut self,
        condition: FilterCondition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let only_empty_row = data.rows.len() == 1 && !data.rows[0].condition(cx).is_complete();
        if only_empty_row {
            if let Some(data) = self.data_mut() {
                data.rows.clear();
            }
        }
        self.add_filter_row(None, Some(condition), window, cx);
        self.apply_filters(None, window, cx);
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

    /// True (with a status message) while the active tab holds edits that
    /// were not sent: a refetch would silently drop them.
    fn blocked_by_staged_edits(&mut self, cx: &mut Context<Self>) -> bool {
        let staged = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.data.as_ref())
            .is_some_and(|d| d.edits.iter().any(|e| !e.applied));
        if staged {
            self.conn.status =
                "Ada perubahan sel yang belum dikirim: klik Apply atau Discard dulu.".to_string();
            cx.notify();
        }
        staged
    }

    /// Apply one row (`only`) or every checked row, then reload the grid.
    pub(super) fn apply_filters(
        &mut self,
        only: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        if self.blocked_by_staged_edits(cx) {
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
        // A new filter starts at the first page.
        self.show_page(0, window, cx);
    }

    /// Jump to a page: `offset` if given, else whatever the Limit and
    /// Offset boxes say (unreadable numbers keep the current value).
    /// The boxes are rewritten with the values actually used.
    pub(super) fn go_page(
        &mut self,
        offset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let offset = offset.unwrap_or_else(|| {
            data.offset_input
                .read(cx)
                .value()
                .trim()
                .parse()
                .unwrap_or(data.page.offset)
        });
        self.show_page(offset, window, cx);
    }

    /// Set the page (clamped) and reload.
    fn show_page(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked_by_staged_edits(cx) {
            return;
        }
        let Some(data) = self.data_mut() else {
            return;
        };
        let limit = data
            .limit_input
            .read(cx)
            .value()
            .trim()
            .parse()
            .unwrap_or(data.page.limit);
        data.page = TablePage::new(offset, limit).with_rowid(data.page.rowid);
        let (limit_input, offset_input, page) = (
            data.limit_input.clone(),
            data.offset_input.clone(),
            data.page,
        );
        Self::set_input(&limit_input, page.limit.to_string(), window, cx);
        Self::set_input(&offset_input, page.offset.to_string(), window, cx);
        self.reload_data(cx);
    }

    /// Drop the applied filter and reset the bar to one empty row.
    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conn.busy {
            return;
        }
        let Some(data) = self.data_mut() else {
            return;
        };
        data.rows.clear();
        data.applied = TableFilter::default();
        self.add_filter_row(None, None, window, cx);
        self.show_page(0, window, cx);
    }

    /// Switch the bottom tab. Structure, Index and DDL are fetched from
    /// the data dictionary each time they are opened, so they never show
    /// stale definitions; Data keeps its rows.
    pub(super) fn set_data_view(&mut self, view: DataView, cx: &mut Context<Self>) {
        let busy = self.conn.busy;
        let Some(data) = self.data_mut() else {
            return;
        };
        if data.view == view {
            return;
        }
        if view != DataView::Data && busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        data.view = view;
        cx.notify();
        if view != DataView::Data {
            self.load_data_view(cx);
        } else if self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.last_result.is_none())
        {
            // Opened on another view (Describe): the rows were never loaded.
            self.reload_data(cx);
        }
    }

    /// (Re)load whatever the active data tab's current view shows.
    pub(super) fn load_data_view(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let (owner, name, kind, view) = (
            data.owner.clone(),
            data.name.clone(),
            data.kind.clone(),
            data.view,
        );
        match view {
            DataView::Data => self.reload_data(cx),
            DataView::Structure => {
                self.conn.status = format!("Memuat struktur {owner}.{name}...");
                self.spawn_db(Job::Describe { owner, name }, PendingOp::DataStructure, cx);
            }
            DataView::Index => {
                self.conn.status = format!("Memuat index {owner}.{name}...");
                self.spawn_db(Job::Indexes { owner, name }, PendingOp::DataIndexes, cx);
            }
            DataView::Ddl => {
                self.conn.status = format!("Memuat DDL {owner}.{name}...");
                self.spawn_db(Job::Ddl { owner, name, kind }, PendingOp::DataDdl, cx);
            }
        }
    }

    /// Fetch the active data tab's table with its applied filter. The
    /// statement goes to the Info log with its bind values.
    pub(super) fn reload_data(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        // A refetch would drop the edits that exist only in the grid.
        let staged = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.data.as_ref())
            .is_some_and(|d| d.edits.iter().any(|e| !e.applied));
        if staged {
            self.conn.status =
                "Ada perubahan sel yang belum dikirim: klik Apply atau Discard dulu.".to_string();
            cx.notify();
            return;
        }
        let Some(data) = self.tabs.get(self.active_tab).and_then(|t| t.data.as_ref()) else {
            return;
        };
        let (owner, name, filter, page) = (
            data.owner.clone(),
            data.name.clone(),
            data.applied.clone(),
            data.page,
        );
        let query = select_sql(&owner, &name, &filter, page);
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
        self.conn.status = format!("Memuat {owner}.{name}...");
        // Not recorded in query history: the statement has bind
        // placeholders, so re-running it from History would fail.
        self.conn.pending_sql = None;
        self.spawn_db(
            Job::QueryTable {
                owner,
                name,
                filter,
                page,
            },
            PendingOp::Run,
            cx,
        );
    }

    /// Change the hidden-column set of the active data tab and redraw
    /// the grid without refetching.
    fn update_hidden(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut HashSet<String>)) {
        let Some(data) = self.data_mut() else {
            return;
        };
        f(&mut data.hidden);
        let hidden = data.hidden.clone();
        let table = self.active_tab().table_state.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().set_hidden(hidden);
            state.refresh(cx);
        });
        cx.notify();
    }

    // ----- rendering -----

    /// A strip above the grid while cell edits wait to be sent or
    /// committed, with Apply / Discard.
    pub(super) fn render_edit_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let data = self.active_tab().data.as_ref()?;
        if data.edits.is_empty() {
            return None;
        }
        let busy = self.conn.busy;
        let staged = data.edits.iter().filter(|e| !e.applied).count();
        let sent = data.edits.len() - staged;
        let label = match (staged, sent) {
            (s, 0) => format!("{s} perubahan belum dikirim ke database"),
            (0, n) => {
                format!("{n} perubahan terkirim, belum di-commit: pakai Commit atau Rollback")
            }
            (s, n) => format!("{s} belum dikirim, {n} terkirim tapi belum di-commit"),
        };
        let tint = if staged > 0 {
            theme.warning
        } else {
            theme.primary
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .px(px(SPACE_3))
                .h(px(32.))
                .bg(tint.opacity(0.14))
                .border_b_1()
                .border_color(theme.border)
                .child(div().size(px(8.)).rounded_full().bg(tint))
                .child(caption_text(label, theme))
                .child(div().flex_1())
                .when(staged > 0, |this| {
                    this.child(
                        Button::new("edits-discard")
                            .small()
                            .label("Discard")
                            .tooltip("Buang perubahan yang belum dikirim")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.discard_edits(cx))),
                    )
                    .child(
                        Button::new("edits-apply")
                            .small()
                            .primary()
                            .label(format!("Apply ({staged})"))
                            .tooltip("Kirim perubahan ke database (belum commit)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.apply_edits(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// The Details panel: every column of the selected row as a
    /// `name / value` list, so a wide row can be read without scrolling
    /// the grid sideways.
    pub(super) fn render_details_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let tab = self.active_tab();
        let selected = tab.table_state.read(cx).selected_row();
        let fields = selected
            .map(|row| tab.table_state.read(cx).delegate().row_fields(row))
            .unwrap_or_default();
        let mut list = div()
            .id("details-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(SPACE_3))
            .py(px(SPACE_2))
            .gap(px(SPACE_2));
        if fields.is_empty() {
            list = list.child(caption_text(
                "Pilih sebuah baris untuk melihat semua kolomnya.".to_string(),
                theme,
            ));
        }
        for (name, value) in fields {
            let is_null = value.is_null();
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pb(px(SPACE_2))
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .child(caption_text(name, theme))
                    .child(
                        div()
                            .font_family(crate::MONO_FONT_FAMILY)
                            .font_features(gpui_kit::FontFeatures::disable_ligatures())
                            .text_size(px(FONT_GRID))
                            .line_height(px(LEADING_GRID))
                            .text_color(if is_null {
                                theme.muted_foreground.opacity(0.6)
                            } else {
                                theme.foreground
                            })
                            .child(if is_null {
                                "NULL".to_string()
                            } else {
                                value.display()
                            }),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .w(px(280.))
            .flex_none()
            .h_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(DATA_HEADER_HEIGHT))
                    .px(px(SPACE_3))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(caption_text(
                        match selected {
                            Some(row) => format!("Details · baris {}", row + 1),
                            None => "Details".to_string(),
                        },
                        theme,
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("details-close")
                            .small()
                            .ghost()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(data) = this.data_mut() {
                                    data.show_details = false;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(list)
            .into_any_element()
    }

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
        // Wraps in a narrow window; the hint truncates first.
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
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
                .child(
                    caption_text(
                        if applied_count == 0 {
                            "Tanpa filter. Enter di kolom nilai = Apply All.".to_string()
                        } else {
                            format!("{applied_count} filter aktif")
                        },
                        theme,
                    )
                    .flex_1()
                    .min_w(px(0.))
                    .truncate(),
                )
                .child(
                    Button::new("filter-clear")
                        .small()
                        .label("Clear")
                        .disabled(self.conn.busy)
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
                        .disabled(self.conn.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.apply_filters(None, window, cx)),
                        ),
                ),
        );
        if data.show_sql {
            let preview = select_sql(&data.owner, &data.name, &data.draft(cx), data.page).preview();
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
        // Never thinner than a usable input: in a narrow window the row
        // wraps instead.
        let value = div()
            .flex_1()
            .min_w(px(120.))
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
            .flex_wrap()
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
                    .disabled(self.conn.busy)
                    .tooltip("Terapkan filter baris ini saja")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_filters(Some(id), window, cx)
                    })),
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

    /// Footer of a data tab: view tabs, paging controls, column picker,
    /// row range and timing.
    pub(super) fn render_data_footer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.active_tab();
        let current = tab.data.as_ref().map_or(DataView::Data, |d| d.view);
        let on_data = current == DataView::Data;
        let page = tab.data.as_ref().map(|d| d.page).unwrap_or_default();
        let mut text = match &tab.last_result {
            Some(result) => {
                let rows = result.row_count();
                let mut text = if rows == 0 {
                    "0 baris".to_string()
                } else {
                    format!("baris {}-{}", page.offset + 1, page.offset + rows)
                };
                if result.truncated {
                    text.push('+');
                }
                text
            }
            None => String::new(),
        };
        if let Some(ms) = tab.elapsed_ms {
            text.push_str(&format!(" · {ms} ms"));
        }
        let more = tab.last_result.as_ref().is_some_and(|r| r.truncated);
        let filters = tab.data.as_ref().map_or(0, |d| d.applied.active().count());
        let busy = self.conn.busy;
        // Wraps onto a second line in a narrow window instead of clipping
        // the paging controls past the edge.
        let mut footer = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .flex_none()
            .items_center()
            .min_h(px(PAGINATION_HEIGHT))
            .gap_x(px(SPACE_2))
            .gap_y(px(SPACE_1))
            .px(px(SPACE_3))
            .py(px(SPACE_1))
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .children(DataView::ALL.map(|view| {
                let active = view == current;
                let button = Button::new(("data-view", view as usize))
                    .small()
                    .label(view.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_data_view(view, cx)));
                if active {
                    button.primary()
                } else {
                    button.ghost()
                }
            }))
            .when(on_data && filters > 0, |this| {
                this.child(icon(IconName::ListFilter, 14., theme.success))
                    .child(caption_text(format!("{filters} filter"), theme))
            })
            .child(div().flex_1());
        if on_data {
            if let Some(data) = tab.data.as_ref() {
                let step = page.limit;
                footer = footer
                    .child(
                        Button::new("page-prev")
                            .small()
                            .icon(IconName::ChevronLeft)
                            .tooltip("Halaman sebelumnya")
                            .disabled(busy || page.offset == 0)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.go_page(Some(page.prev().offset), window, cx)
                            })),
                    )
                    .child(caption_text("Limit".to_string(), theme))
                    .child(
                        div()
                            .flex_none()
                            .w(px(64.))
                            .child(Input::new(&data.limit_input).small().w_full()),
                    )
                    .child(caption_text("Offset".to_string(), theme))
                    .child(
                        div()
                            .flex_none()
                            .w(px(80.))
                            .child(Input::new(&data.offset_input).small().w_full()),
                    )
                    .child(
                        Button::new("page-go")
                            .small()
                            .label("Go")
                            .tooltip(format!(
                                "Ambil {step} baris mulai dari offset (maks {MAX_PAGE_LIMIT})"
                            ))
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.go_page(None, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("page-next")
                            .small()
                            .icon(IconName::ChevronRight)
                            .tooltip("Halaman berikutnya")
                            .disabled(busy || !more)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.go_page(Some(page.next().offset), window, cx)
                            })),
                    )
                    .child(
                        Button::new("details-toggle")
                            .small()
                            .icon(IconName::PanelRight)
                            .selected(data.show_details)
                            .tooltip("Tampilkan semua kolom baris terpilih")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(data) = this.data_mut() {
                                    data.show_details = !data.show_details;
                                }
                                cx.notify();
                            })),
                    )
                    .child(self.render_columns_button(data, theme, cx))
                    .child(
                        div()
                            .flex_none()
                            .w(px(130.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(caption_text(text, theme)),
                    );
            }
        }
        footer
    }

    /// The Columns dropdown: tick which columns the grid shows. The last
    /// visible column cannot be hidden.
    fn render_columns_button(
        &self,
        data: &DataTab,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let columns = data.columns.clone();
        let hidden = data.hidden.clone();
        let hidden_count = columns.iter().filter(|c| hidden.contains(*c)).count();
        let label = if hidden_count == 0 {
            "Columns".to_string()
        } else {
            format!(
                "Columns ({} dari {})",
                columns.len() - hidden_count,
                columns.len()
            )
        };
        Button::new("columns-picker")
            .small()
            .child(select_trigger(label, theme))
            .tooltip("Pilih kolom yang ditampilkan")
            .disabled(columns.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(220.));
                for column in &columns {
                    let visible = !hidden.contains(column);
                    let shown =
                        columns.len() - columns.iter().filter(|c| hidden.contains(*c)).count();
                    let (view, column) = (view.clone(), column.clone());
                    menu = menu.item(
                        PopupMenuItem::new(column.clone())
                            .checked(visible)
                            .on_click(move |_, _, cx: &mut App| {
                                // Never hide the last visible column.
                                if visible && shown <= 1 {
                                    return;
                                }
                                let column = column.clone();
                                view.update(cx, |this, cx| {
                                    this.update_hidden(cx, |set| {
                                        if !set.remove(&column) {
                                            set.insert(column);
                                        }
                                    });
                                });
                            }),
                    );
                }
                menu = menu.separator();
                let all = view.clone();
                menu.item(PopupMenuItem::new("Tampilkan semua").on_click(
                    move |_, _, cx: &mut App| {
                        all.update(cx, |this, cx| this.update_hidden(cx, |set| set.clear()));
                    },
                ))
            })
    }
}
