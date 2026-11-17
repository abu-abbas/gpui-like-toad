//! [`TableDelegate`] adapter that renders an [`ora_core::QueryResult`] in a
//! gpui-kit [`DataTable`]. The workbench swaps results in place via
//! `delegate_mut()` + `refresh()` so the table state (and its scroll
//! position) survives across runs.
//!
//! Pagination is client-side: the delegate shows one [`PAGE_SIZE`]-row
//! window of the full result. Sorting sorts the whole result first, so
//! page boundaries stay consistent while paging.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, Styled as _, Window,
};
use ora_core::{FilterCondition, FilterOp, QueryResult, Value, ROWID_COLUMN};

use crate::theme::{FONT_GRID, LEADING_GRID};

/// Rows per grid page (design-system pagination: "10 / page").
pub const PAGE_SIZE: usize = 10;

/// Width of the row-number gutter: fits "1000" at the grid font.
const GUTTER_WIDTH: f32 = 52.;

pub struct ResultsDelegate {
    result: QueryResult,
    columns: Vec<Column>,
    /// Per result column: right-align its cells (numeric types).
    numeric: Vec<bool>,
    /// Result columns hidden by name (the Columns picker); kept across
    /// fetches so a refresh does not bring them back.
    hidden: HashSet<String>,
    /// Result column behind each shown data column, in order.
    visible: Vec<usize>,
    /// Show a fixed row-number column on the left (data tabs).
    row_numbers: bool,
    page: usize,
    page_size: usize,
    /// Table column of the last right mouse-down on a cell: the table's
    /// context menu only reports the row.
    right_col: Option<usize>,
    /// Set on data tabs: called when a Quick Filter menu entry is picked.
    on_quick_filter: Option<QuickFilterHandler>,
    /// Set on editable data tabs: called for double click / "Edit nilai".
    on_edit: Option<EditHandler>,
    /// Result column holding each row's ROWID (never shown), if fetched.
    rowid_col: Option<usize>,
    /// Edited cells by (ROWID, column name), tinted until commit.
    pending: HashMap<(String, String), CellMark>,
}

pub type QuickFilterHandler = Rc<dyn Fn(FilterCondition, &mut Window, &mut App)>;

/// A request to edit one cell: its row, column and current value.
#[derive(Clone, Debug, PartialEq)]
pub struct CellEditRequest {
    pub rowid: String,
    pub column: String,
    pub value: Value,
}

pub type EditHandler = Rc<dyn Fn(CellEditRequest, &mut Window, &mut App)>;

/// Where an edited cell stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellMark {
    /// Changed in the grid only; not sent to the database yet.
    Staged,
    /// Sent (UPDATE ran) but not committed yet.
    Applied,
}

/// Longest cell value shown inside a Quick Filter menu label.
const MENU_VALUE_CHARS: usize = 32;

/// The Quick Filter entries for one cell, in menu order; `None` is a
/// separator. A NULL cell only offers the NULL checks (comparing with
/// NULL never matches).
pub fn quick_filter_entries(column: &str, value: &Value) -> Vec<Option<(String, FilterCondition)>> {
    let null_checks = [FilterOp::IsNull, FilterOp::IsNotNull].map(|op| {
        Some((
            format!("{column} {}", op.label()),
            FilterCondition::new(column, op, ""),
        ))
    });
    if value.is_null() {
        return null_checks.into();
    }
    let text = value.display();
    let mut shown: String = text.chars().take(MENU_VALUE_CHARS).collect();
    if shown.len() < text.len() {
        shown.push('…');
    }
    let groups: [&[FilterOp]; 4] = [
        &[
            FilterOp::Eq,
            FilterOp::Ne,
            FilterOp::Lt,
            FilterOp::Gt,
            FilterOp::Le,
            FilterOp::Ge,
        ],
        &[FilterOp::Contains, FilterOp::NotContains],
        &[FilterOp::StartsWith, FilterOp::EndsWith],
        &[FilterOp::In, FilterOp::NotIn],
    ];
    let mut entries = Vec::new();
    for group in groups {
        for op in group {
            let label = match op {
                FilterOp::In | FilterOp::NotIn => format!("{column} {} ({shown})", op.label()),
                _ => format!("{column} {} '{shown}'", op.label()),
            };
            entries.push(Some((
                label,
                FilterCondition::new(column, *op, text.clone()),
            )));
        }
        entries.push(None);
    }
    entries.extend(null_checks);
    entries
}

/// Whether a database type name is numeric, e.g. "NUMBER(8,2)".
fn is_numeric_type(data_type: &str) -> bool {
    let t = data_type.trim().to_ascii_uppercase();
    [
        "NUMBER", "FLOAT", "BINARY_", "INTEGER", "DECIMAL", "NUMERIC",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// Right-align a column when its declared type is numeric, or when every
/// non-NULL value in it is a number (expressions report no useful type).
fn numeric_columns(result: &QueryResult) -> Vec<bool> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(ix, col)| {
            if is_numeric_type(&col.data_type) {
                return true;
            }
            let mut seen = false;
            for row in &result.rows {
                match row.get(ix) {
                    Some(Value::Integer(_) | Value::Float(_)) => seen = true,
                    Some(Value::Null) | None => {}
                    Some(_) => return false,
                }
            }
            seen
        })
        .collect()
}

impl ResultsDelegate {
    pub fn empty() -> Self {
        ResultsDelegate {
            result: QueryResult::default(),
            columns: Vec::new(),
            numeric: Vec::new(),
            hidden: HashSet::new(),
            visible: Vec::new(),
            row_numbers: false,
            page: 0,
            page_size: PAGE_SIZE,
            right_col: None,
            on_quick_filter: None,
            on_edit: None,
            rowid_col: None,
            pending: HashMap::new(),
        }
    }

    /// Rows per page. Query tabs keep the compact 10; data tabs, where the
    /// grid fills the whole work area, use a larger page.
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    /// Add the row-number gutter as a fixed first column.
    pub fn with_row_numbers(mut self) -> Self {
        self.row_numbers = true;
        self
    }

    /// Offer Quick Filter in the cell context menu.
    pub fn with_quick_filter(mut self, handler: QuickFilterHandler) -> Self {
        self.on_quick_filter = Some(handler);
        self
    }

    /// Let cells be edited (double click or the context menu).
    pub fn with_cell_edit(mut self, handler: EditHandler) -> Self {
        self.on_edit = Some(handler);
        self
    }

    /// Replace the set of tinted (edited) cells.
    pub fn set_pending(&mut self, pending: HashMap<(String, String), CellMark>) {
        self.pending = pending;
    }

    /// Show `value` in the cell of the row with `rowid` (an edit that has
    /// not been refetched). Returns the value it replaced.
    pub fn set_cell(&mut self, rowid: &str, column: &str, value: Value) -> Option<Value> {
        let rowid_col = self.rowid_col?;
        let col = self.result.columns.iter().position(|c| c.name == column)?;
        let row = self
            .result
            .rows
            .iter_mut()
            .find(|r| r.get(rowid_col).is_some_and(|v| v.display() == rowid))?;
        Some(std::mem::replace(&mut row[col], value))
    }

    /// The edit request for the cell at result row `abs_row`, column
    /// `data_col`; `None` when editing is off or the row has no ROWID.
    fn edit_request(&self, abs_row: usize, data_col: usize) -> Option<CellEditRequest> {
        self.on_edit.as_ref()?;
        let rowid = self.result.get(abs_row, self.rowid_col?)?.display();
        Some(CellEditRequest {
            rowid,
            column: self.result.columns.get(data_col)?.name.clone(),
            value: self.result.get(abs_row, data_col)?.clone(),
        })
    }

    /// Table column index to result column index; `None` is the gutter.
    fn data_col(&self, col_ix: usize) -> Option<usize> {
        let ix = if self.row_numbers {
            col_ix.checked_sub(1)?
        } else {
            col_ix
        };
        self.visible.get(ix).copied()
    }

    /// Hide the named result columns. Takes effect on the current result
    /// right away; the caller refreshes the table state.
    pub fn set_hidden(&mut self, hidden: HashSet<String>) {
        self.hidden = hidden;
        self.rebuild_columns();
    }

    fn rebuild_columns(&mut self) {
        self.columns = Vec::with_capacity(self.result.columns.len() + 1);
        if self.row_numbers {
            self.columns.push(
                Column::new("rownum", "#")
                    .width(GUTTER_WIDTH)
                    .fixed_left()
                    .text_right()
                    .resizable(false)
                    .movable(false)
                    .selectable(false),
            );
        }
        self.visible.clear();
        for (i, c) in self.result.columns.iter().enumerate() {
            if c.name == ROWID_COLUMN || self.hidden.contains(&c.name) {
                continue;
            }
            self.visible.push(i);
            let column = Column::new(format!("c{i}"), c.name.clone())
                .width(140.)
                .sortable();
            self.columns.push(if self.numeric.get(i) == Some(&true) {
                column.text_right()
            } else {
                column
            });
        }
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn set_result(&mut self, result: QueryResult) {
        self.numeric = numeric_columns(&result);
        self.rowid_col = result.columns.iter().position(|c| c.name == ROWID_COLUMN);
        self.result = result;
        self.rebuild_columns();
        self.page = 0;
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(self.total_pages().saturating_sub(1));
    }

    pub fn total_rows(&self) -> usize {
        self.result.rows.len()
    }

    pub fn total_pages(&self) -> usize {
        self.result.rows.len().div_ceil(self.page_size)
    }

    /// 1-based (first, last, total) row numbers for the current page's
    /// "1–10 of N" label. Empty results report (0, 0, 0).
    pub fn page_range(&self) -> (usize, usize, usize) {
        let total = self.total_rows();
        if total == 0 {
            return (0, 0, 0);
        }
        let first = self.page * self.page_size + 1;
        let last = ((self.page + 1) * self.page_size).min(total);
        (first, last, total)
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize) -> String {
        self.result
            .get(row_ix, col_ix)
            .map(|v| v.display())
            .unwrap_or_default()
    }

    /// Map a visible row index to the underlying result row.
    fn absolute_row(&self, row_ix: usize) -> usize {
        self.page * self.page_size + row_ix
    }
}

fn cmp_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => match (x, y) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Greater,
            (_, Value::Null) => Ordering::Less,
            (Value::Integer(m), Value::Integer(n)) => m.cmp(n),
            (Value::Float(m), Value::Float(n)) => m.partial_cmp(n).unwrap_or(Ordering::Equal),
            (Value::Integer(m), Value::Float(n)) => {
                (*m as f64).partial_cmp(n).unwrap_or(Ordering::Equal)
            }
            (Value::Float(m), Value::Integer(n)) => {
                m.partial_cmp(&(*n as f64)).unwrap_or(Ordering::Equal)
            }
            (Value::Bytes(m), Value::Bytes(n)) => m.cmp(n),
            _ => x.display().cmp(&y.display()),
        },
    }
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.total_rows()
            .saturating_sub(self.page * self.page_size)
            .min(self.page_size)
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Header-only column dividers: the body has none, and the table's
        // own handles only show on hover.
        let divider = cx.theme().border;
        div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .border_l_1()
            .border_color(divider)
            .pl(px(4.))
            .when(
                self.columns[col_ix].align == gpui_kit::TextAlign::Right,
                |this| this.justify_end(),
            )
            .child(self.columns[col_ix].name.clone())
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Explicit 12px/20px: the table's own cell size would render 14px
        // text, but the spec fixes the data grid at 12px. Monospace keeps
        // digits and codes aligned down a column.
        let cell = div()
            .w_full()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, _| {
                    table.delegate_mut().right_col = Some(col_ix);
                }),
            )
            .font_family(crate::MONO_FONT_FAMILY)
            .font_features(gpui_kit::FontFeatures::disable_ligatures())
            .text_size(px(FONT_GRID))
            .line_height(px(LEADING_GRID))
            .truncate();
        let abs_row = self.absolute_row(row_ix);
        let Some(data_col) = self.data_col(col_ix) else {
            return cell
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child((abs_row + 1).to_string());
        };
        let request = self.edit_request(abs_row, data_col);
        // Edited cells are tinted: amber until sent, blue once sent but
        // not yet committed.
        let mark = request
            .as_ref()
            .and_then(|r| self.pending.get(&(r.rowid.clone(), r.column.clone())))
            .copied();
        let theme = cx.theme();
        let cell = match mark {
            Some(CellMark::Staged) => cell.bg(theme.warning.opacity(0.28)),
            Some(CellMark::Applied) => cell.bg(theme.primary.opacity(0.22)),
            None => cell,
        };
        let cell = match (self.on_edit.clone(), request) {
            (Some(handler), Some(request)) => cell.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, event: &gpui_kit::MouseDownEvent, window, cx| {
                    if event.click_count >= 2 {
                        handler(request.clone(), window, cx);
                    }
                }),
            ),
            _ => cell,
        };
        let is_null = self
            .result
            .get(abs_row, data_col)
            .is_some_and(Value::is_null);
        if is_null {
            // NULL is not an empty string: show it, dimmed.
            return cell
                .text_color(cx.theme().muted_foreground.opacity(0.6))
                .child("NULL");
        }
        cell.when(self.numeric.get(data_col) == Some(&true), |c| {
            c.text_right()
        })
        .child(self.cell_text(abs_row, data_col))
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(data_col) = self.right_col.and_then(|c| self.data_col(c)) else {
            return menu;
        };
        let abs_row = self.absolute_row(row_ix);
        if let (Some(handler), Some(request)) =
            (self.on_edit.clone(), self.edit_request(abs_row, data_col))
        {
            menu = menu.item(
                PopupMenuItem::new("Edit nilai…").on_click(move |_, window, cx| {
                    handler(request.clone(), window, cx);
                }),
            );
            if self.on_quick_filter.is_some() {
                menu = menu.separator();
            }
        }
        let Some(handler) = self.on_quick_filter.clone() else {
            return menu;
        };
        let (Some(column), Some(value)) = (
            self.result.columns.get(data_col),
            self.result.get(abs_row, data_col),
        ) else {
            return menu;
        };
        menu = menu.min_w(px(260.)).label("Quick Filter");
        for entry in quick_filter_entries(&column.name, value) {
            menu = match entry {
                None => menu.separator(),
                Some((label, condition)) => {
                    let handler = handler.clone();
                    menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        handler(condition.clone(), window, cx);
                    }))
                }
            };
        }
        menu
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        let abs_row = self.absolute_row(row_ix);
        Some(match self.data_col(col_ix) {
            Some(data_col) => self.cell_text(abs_row, data_col).into(),
            None => (abs_row + 1).to_string().into(),
        })
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        let Some(col_ix) = self.data_col(col_ix) else {
            return;
        };
        match sort {
            ColumnSort::Ascending => self
                .result
                .rows
                .sort_by(|a, b| cmp_values(a.get(col_ix), b.get(col_ix))),
            ColumnSort::Descending => self
                .result
                .rows
                .sort_by(|a, b| cmp_values(b.get(col_ix), a.get(col_ix))),
            // Default keeps the current order: the original fetch order is
            // not retained after a sort, so there is nothing to restore to.
            ColumnSort::Default => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ora_core::ColumnInfo;

    fn int(n: i128) -> Value {
        Value::Integer(n)
    }

    fn float(n: f64) -> Value {
        Value::Float(n)
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn sort_puts_nulls_last_and_orders_numbers() {
        assert_eq!(
            cmp_values(Some(&Value::Null), Some(&int(1))),
            Ordering::Greater
        );
        assert_eq!(
            cmp_values(Some(&int(1)), Some(&Value::Null)),
            Ordering::Less
        );
        assert_eq!(
            cmp_values(Some(&Value::Null), Some(&Value::Null)),
            Ordering::Equal
        );
        assert_eq!(cmp_values(Some(&int(2)), Some(&int(10))), Ordering::Less);
        assert_eq!(
            cmp_values(Some(&int(10)), Some(&float(2.5))),
            Ordering::Greater
        );
        assert_eq!(cmp_values(Some(&float(2.5)), Some(&int(3))), Ordering::Less);
        assert_eq!(cmp_values(None, Some(&int(1))), Ordering::Greater);
        assert_eq!(cmp_values(Some(&int(1)), None), Ordering::Less);
        assert_eq!(cmp_values(None, None), Ordering::Equal);
    }

    #[test]
    fn sort_falls_back_to_display_text_for_mixed_types() {
        assert_eq!(
            cmp_values(Some(&text("a")), Some(&text("b"))),
            Ordering::Less
        );
        // Integer vs Text compares display strings: "10" < "2" lexicographically.
        assert_eq!(cmp_values(Some(&int(10)), Some(&text("2"))), Ordering::Less);
        assert_eq!(
            cmp_values(Some(&Value::Bytes(vec![1])), Some(&Value::Bytes(vec![2]))),
            Ordering::Less
        );
    }

    fn result_with_rows(n: usize) -> QueryResult {
        QueryResult {
            columns: vec![ColumnInfo {
                name: "N".into(),
                data_type: "NUMBER".into(),
            }],
            rows: (0..n).map(|i| vec![Value::Integer(i as i128)]).collect(),
            truncated: false,
            elapsed: Default::default(),
        }
    }

    #[test]
    fn pagination_windows_rows_ten_per_page() {
        let mut delegate = ResultsDelegate::empty();
        delegate.set_result(result_with_rows(25));
        assert_eq!(delegate.total_pages(), 3);
        assert_eq!(delegate.page_range(), (1, 10, 25));
        delegate.set_page(1);
        assert_eq!(delegate.page_range(), (11, 20, 25));
        delegate.set_page(2);
        assert_eq!(delegate.page_range(), (21, 25, 25));
        // Out-of-range pages clamp to the last page.
        delegate.set_page(99);
        assert_eq!(delegate.page(), 2);
        // A new result resets to page 0.
        delegate.set_result(result_with_rows(5));
        assert_eq!(delegate.page(), 0);
        assert_eq!(delegate.total_pages(), 1);
        assert_eq!(delegate.page_range(), (1, 5, 5));
        delegate.set_result(result_with_rows(0));
        assert_eq!(delegate.total_pages(), 0);
        assert_eq!(delegate.page_range(), (0, 0, 0));
    }

    #[test]
    fn numeric_columns_by_type_or_by_values() {
        assert!(is_numeric_type("NUMBER(8,2)"));
        assert!(is_numeric_type("binary_double"));
        assert!(!is_numeric_type("VARCHAR2(20)"));
        let col = |name: &str, data_type: &str| ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
        };
        let result = QueryResult {
            columns: vec![col("A", "NUMBER"), col("B", ""), col("C", ""), col("D", "")],
            rows: vec![
                vec![text("1"), int(1), text("x"), Value::Null],
                vec![Value::Null, Value::Null, int(2), Value::Null],
            ],
            truncated: false,
            elapsed: Default::default(),
        };
        // A by type, B by values, C mixed, D all NULL.
        assert_eq!(numeric_columns(&result), vec![true, true, false, false]);
    }

    #[test]
    fn quick_filter_entries_cover_all_operators() {
        let entries = quick_filter_entries("STATUS", &text("shipped"));
        let labels: Vec<&str> = entries
            .iter()
            .flatten()
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(labels.len(), FilterOp::ALL.len());
        assert_eq!(labels[0], "STATUS = 'shipped'");
        assert!(labels.contains(&"STATUS Has prefix 'shipped'"));
        assert!(labels.contains(&"STATUS NOT IN (shipped)"));
        assert_eq!(labels[13], "STATUS IS NOT NULL");
        let (_, eq) = entries[0].as_ref().unwrap();
        assert_eq!(*eq, FilterCondition::new("STATUS", FilterOp::Eq, "shipped"));

        // NULL cells only get the NULL checks.
        let nulls = quick_filter_entries("X", &Value::Null);
        assert_eq!(nulls.len(), 2);

        // Long values are shortened in the label, not in the condition.
        let long = "a".repeat(100);
        let entries = quick_filter_entries("C", &text(&long));
        let (label, condition) = entries[0].as_ref().unwrap();
        assert!(label.ends_with("…'") && label.len() < 60);
        assert_eq!(condition.value, long);
    }

    #[test]
    fn hidden_columns_are_skipped_and_survive_new_results() {
        let two_columns = || QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "A".into(),
                    data_type: "NUMBER".into(),
                },
                ColumnInfo {
                    name: "B".into(),
                    data_type: "VARCHAR2".into(),
                },
                ColumnInfo {
                    name: "C".into(),
                    data_type: "VARCHAR2".into(),
                },
            ],
            rows: vec![vec![int(1), text("x"), text("y")]],
            truncated: false,
            elapsed: Default::default(),
        };
        let mut delegate = ResultsDelegate::empty().with_row_numbers();
        delegate.set_result(two_columns());
        assert_eq!(delegate.columns.len(), 4);
        delegate.set_hidden(HashSet::from(["B".to_string()]));
        // Gutter + A + C; table column 2 is result column 2 (C).
        assert_eq!(delegate.columns.len(), 3);
        assert_eq!(delegate.data_col(0), None);
        assert_eq!(delegate.data_col(1), Some(0));
        assert_eq!(delegate.data_col(2), Some(2));
        assert_eq!(delegate.data_col(3), None);
        // A refetch keeps B hidden.
        delegate.set_result(two_columns());
        assert_eq!(delegate.columns.len(), 3);
        delegate.set_hidden(HashSet::new());
        assert_eq!(delegate.columns.len(), 4);
    }

    #[test]
    fn row_number_gutter_shifts_data_columns() {
        let mut delegate = ResultsDelegate::empty().with_row_numbers();
        delegate.set_result(result_with_rows(3));
        assert_eq!(delegate.columns.len(), 2);
        assert_eq!(delegate.data_col(0), None);
        assert_eq!(delegate.data_col(1), Some(0));
        assert_eq!(ResultsDelegate::empty().data_col(0), None);
    }
}
