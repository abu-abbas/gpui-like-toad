//! [`TableDelegate`] adapter that renders an [`ora_core::QueryResult`] in a
//! gpui-kit [`DataTable`]. The workbench swaps results in place via
//! `delegate_mut()` + `refresh()` so the table state (and its scroll
//! position) survives across runs.
//!
//! Pagination is client-side: the delegate shows one [`PAGE_SIZE`]-row
//! window of the full result. Sorting sorts the whole result first, so
//! page boundaries stay consistent while paging.

use std::cmp::Ordering;

use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::{
    div, px, App, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
};
use ora_core::{QueryResult, Value};

use crate::theme::{FONT_GRID, LEADING_GRID};

/// Rows per grid page (design-system pagination: "10 / page").
pub const PAGE_SIZE: usize = 10;

pub struct ResultsDelegate {
    result: QueryResult,
    columns: Vec<Column>,
    page: usize,
    page_size: usize,
}

impl ResultsDelegate {
    pub fn empty() -> Self {
        ResultsDelegate {
            result: QueryResult::default(),
            columns: Vec::new(),
            page: 0,
            page_size: PAGE_SIZE,
        }
    }

    /// Rows per page. Query tabs keep the compact 10; data tabs, where the
    /// grid fills the whole work area, use a larger page.
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn set_result(&mut self, result: QueryResult) {
        self.columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                Column::new(format!("c{i}"), c.name.clone())
                    .width(140.)
                    .sortable()
            })
            .collect();
        self.result = result;
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

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Explicit 12px/20px: the table's own Small cell size would render
        // 14px text, but the spec fixes the data grid at 12px.
        div()
            .text_size(px(FONT_GRID))
            .line_height(px(LEADING_GRID))
            .truncate()
            .child(self.cell_text(self.absolute_row(row_ix), col_ix))
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(self.cell_text(self.absolute_row(row_ix), col_ix).into())
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
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
}
