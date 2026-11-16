//! [`TableDelegate`] adapter that renders an [`ora_core::QueryResult`] in a
//! gpui-kit [`DataTable`]. The workbench swaps results in place via
//! `delegate_mut()` + `refresh()` so the table state (and its scroll
//! position) survives across runs.

use std::cmp::Ordering;

use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::{App, Context, IntoElement, SharedString, Window};
use ora_core::{QueryResult, Value};

pub struct ResultsDelegate {
    result: QueryResult,
    columns: Vec<Column>,
}

impl ResultsDelegate {
    pub fn empty() -> Self {
        ResultsDelegate {
            result: QueryResult::default(),
            columns: Vec::new(),
        }
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
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize) -> String {
        self.result
            .get(row_ix, col_ix)
            .map(|v| v.display())
            .unwrap_or_default()
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
        self.result.rows.len()
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
        self.cell_text(row_ix, col_ix)
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(self.cell_text(row_ix, col_ix).into())
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
