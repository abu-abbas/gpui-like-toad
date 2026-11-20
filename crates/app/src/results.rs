//! [`TableDelegate`] adapter that renders an [`ora_core::QueryResult`] in a
//! gpui-kit [`DataTable`]. The workbench swaps results in place via
//! `delegate_mut()` + `refresh()` so the table state (and its scroll
//! position) survives across runs.
//!
//! Pagination is client-side: the delegate shows one [`PAGE_SIZE`]-row
//! window of the full result. Sorting sorts the whole result first, so
//! page boundaries stay consistent while paging.
//!
//! The delegate shares the tab's result (`Arc`) instead of owning a copy:
//! a sort only keeps a row order (one index per row) and an edit shown in
//! the grid only an override for that cell, so showing a result costs the
//! view, not a second copy of every cell.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Icon;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, ClipboardItem, Context, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, Styled as _, Window,
};
use ora_core::{FilterCondition, FilterOp, QueryResult, Value, ROWID_COLUMN};

use crate::theme::{FONT_GRID, LEADING_GRID};

mod cell_range;

/// Rows per grid page (design-system pagination: "10 / page").
pub const PAGE_SIZE: usize = 10;

/// Width of the row-number gutter: fits "1000" at the grid font.
const GUTTER_WIDTH: f32 = 52.;

pub type InsertRow = Vec<(String, gpui_kit::Entity<InputState>)>;
pub type InsertRows = Rc<Vec<InsertRow>>;

pub struct ResultsDelegate {
    pub(crate) insert_rows: InsertRows,
    pub(crate) insert_disabled: bool,
    result: Arc<QueryResult>,
    /// Result row behind each grid row after a sort; `None` = fetch order.
    order: Option<Vec<usize>>,
    /// Cells changed in the grid (edits not refetched yet), by (result
    /// row, result column), shown over the shared result.
    overrides: HashMap<(usize, usize), Value>,
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
    selected_rows: std::collections::BTreeSet<usize>,
    selection_anchor: usize,
    /// Picked cell range on data grids (see `cell_range`): where the
    /// pick started and its far corner, as (result row, shown column).
    cell_anchor: Option<cell_range::CellPos>,
    cell_focus: Option<cell_range::CellPos>,
    page: usize,
    page_size: usize,
    /// Table column of the last right mouse-down on a cell: the table's
    /// context menu only reports the row.
    right_col: Option<usize>,
    /// Set on data tabs: called when a Quick Filter menu entry is picked.
    on_quick_filter: Option<QuickFilterHandler>,
    /// Set on editable data tabs: called for double click / "Edit nilai".
    on_edit: Option<EditHandler>,
    /// Set on editable data tabs: the context menu's "Paste" into the
    /// picked cells.
    on_cell_paste: Option<PasteHandler>,
    /// Set on editable data tabs: the context menu's "Delete row(s)",
    /// called with the ROWIDs to stage for deletion.
    on_delete_rows: Option<DeleteRowsHandler>,
    /// Set on editable data tabs: the context menu's "Duplicate row(s)".
    on_duplicate_rows: Option<DuplicateRowsHandler>,
    /// Set on data tabs with ROWIDs: "Load full value" on a clipped cell.
    on_load_cell: Option<LoadCellHandler>,
    /// Set on data tabs with ROWIDs: called with the grid rows now on
    /// screen, so CLOB starts can be read for them only.
    on_visible_rows: Option<VisibleRowsHandler>,
    /// CLOB cells (result row, result column) whose start was already
    /// asked for: each is read at most once per result.
    previews_asked: HashSet<(usize, usize)>,
    /// Table rows last reported as shown.
    last_visible: std::ops::Range<usize>,
    /// Result column holding each row's ROWID (never shown), if fetched.
    rowid_col: Option<usize>,
    /// Edited cells by (ROWID, column name), tinted until commit.
    pending: HashMap<(String, String), CellMark>,
    /// Quoted table name for "Copy As" statements; a placeholder when the
    /// grid shows a free query.
    table_name: Option<String>,
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

pub type PasteHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// "Load full value": called with the row's ROWID and the column name.
pub type LoadCellHandler = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
/// CLOB cells of some rows whose start is to be read.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewBatch {
    pub rowids: Vec<String>,
    pub columns: Vec<String>,
}

/// Called with the table rows (page-relative) the grid now shows.
pub type VisibleRowsHandler = Rc<dyn Fn(std::ops::Range<usize>, &mut Window, &mut App)>;
pub type DeleteRowsHandler = Rc<dyn Fn(Vec<String>, &mut Window, &mut App)>;

/// Rows to duplicate, each as (column, value) pairs without the ROWID.
pub type DuplicateRowsHandler = Rc<dyn Fn(Vec<Vec<(String, String)>>, &mut Window, &mut App)>;

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
/// NULL never matches). A clipped cell (only a preview or just the length
/// is in memory, shown as e.g. "(CLOB, 415 chars)") offers the NULL checks
/// too: its display text is a placeholder, not data, so comparing with it
/// would never match. After "Load full value" the cell holds the real value
/// and gets the full menu.
pub fn quick_filter_entries(column: &str, value: &Value) -> Vec<Option<(String, FilterCondition)>> {
    let null_checks = [FilterOp::IsNull, FilterOp::IsNotNull].map(|op| {
        Some((
            format!("{column} {}", op.label()),
            FilterCondition::new(column, op, ""),
        ))
    });
    if value.is_null() || value.is_clipped() {
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
            insert_rows: Default::default(),
            insert_disabled: false,
            result: Arc::default(),
            order: None,
            overrides: HashMap::new(),
            columns: Vec::new(),
            numeric: Vec::new(),
            hidden: HashSet::new(),
            visible: Vec::new(),
            row_numbers: false,
            selected_rows: Default::default(),
            selection_anchor: 0,
            cell_anchor: None,
            cell_focus: None,
            page: 0,
            page_size: PAGE_SIZE,
            right_col: None,
            on_quick_filter: None,
            on_edit: None,
            on_delete_rows: None,
            on_duplicate_rows: None,
            on_load_cell: None,
            on_cell_paste: None,
            on_visible_rows: None,
            previews_asked: HashSet::new(),
            last_visible: 0..0,
            rowid_col: None,
            table_name: None,
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

    /// Offer "Paste" (into the picked cells) in the cell context menu.
    pub fn with_cell_paste(mut self, handler: PasteHandler) -> Self {
        self.on_cell_paste = Some(handler);
        self
    }

    /// Offer "Delete row" (the selected rows, or the clicked one) in the
    /// cell context menu.
    pub fn with_delete_rows(mut self, handler: DeleteRowsHandler) -> Self {
        self.on_delete_rows = Some(handler);
        self
    }

    /// Offer "Duplicate row" (the selected rows, or the clicked one) in the
    /// cell context menu.
    pub fn with_duplicate_rows(mut self, handler: DuplicateRowsHandler) -> Self {
        self.on_duplicate_rows = Some(handler);
        self
    }

    /// Offer "Load full value" on clipped cells (needs a ROWID column).
    pub fn with_load_cell(mut self, handler: LoadCellHandler) -> Self {
        self.on_load_cell = Some(handler);
        self
    }

    /// Read CLOB starts only for the rows on screen: the handler gets the
    /// visible table rows whenever they change.
    pub fn with_visible_rows(mut self, handler: VisibleRowsHandler) -> Self {
        self.on_visible_rows = Some(handler);
        self
    }

    /// Absolute rows a right click on `abs_row` acts on, in grid order.
    fn target_rows(&self, abs_row: usize) -> Vec<usize> {
        if self.selected_rows.contains(&abs_row) {
            let mut rows: Vec<usize> = self.selected_rows.iter().copied().collect();
            rows.sort_unstable();
            return rows;
        }
        vec![abs_row]
    }

    /// Every field of result row `abs_row` as text, without the ROWID.
    fn row_pairs(&self, abs_row: usize) -> Vec<(String, String)> {
        self.result
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name != ROWID_COLUMN)
            .filter_map(|(ix, c)| Some((c.name.clone(), self.cell(abs_row, ix)?.display())))
            .collect()
    }

    /// ROWIDs a right click on result row `abs_row` deletes: the whole
    /// selection when that row is part of it, otherwise just that row.
    fn delete_targets(&self, abs_row: usize) -> Vec<String> {
        if self.selected_rows.contains(&abs_row) {
            return self.selected_rowids();
        }
        self.rowid_col
            .and_then(|c| self.cell(abs_row, c))
            .map(|v| vec![v.display()])
            .unwrap_or_default()
    }

    /// Replace the set of tinted (edited) cells.
    pub fn set_pending(&mut self, pending: HashMap<(String, String), CellMark>) {
        self.pending = pending;
    }

    /// Memory held by cells changed in the grid ("Load full value" results
    /// and edits). The shared result does not include them, so the all-tabs
    /// budget adds this on top.
    pub fn override_bytes(&self) -> usize {
        self.overrides.values().map(Value::heap_bytes).sum()
    }

    /// (result row, result column) of the cell of the row with `rowid`.
    fn find_cell(&self, rowid: &str, column: &str) -> Option<(usize, usize)> {
        let rowid_col = self.rowid_col?;
        let col = self.result.columns.iter().position(|c| c.name == column)?;
        let row = (0..self.result.rows.len()).find(|&r| {
            self.source_cell(r, rowid_col)
                .is_some_and(|v| v.display() == rowid)
        })?;
        Some((row, col))
    }

    /// Length of the full value behind a clipped cell (a lower bound of the
    /// bytes it takes: characters can be several bytes), `None` when the
    /// cell is not clipped or not in the grid.
    pub fn clipped_len(&self, rowid: &str, column: &str) -> Option<u64> {
        let (row, col) = self.find_cell(rowid, column)?;
        match self.source_cell(row, col)? {
            Value::Clipped(c) => Some(c.full_len),
            _ => None,
        }
    }

    /// Show `value` in the cell of the row with `rowid` (an edit that has
    /// not been refetched). Returns the value it replaced.
    pub fn set_cell(&mut self, rowid: &str, column: &str, value: Value) -> Option<Value> {
        let (row, col) = self.find_cell(rowid, column)?;
        let old = self.source_cell(row, col)?.clone();
        self.overrides.insert((row, col), value);
        Some(old)
    }

    /// The next CLOB previews to read: unread CLOB cells of the shown
    /// columns in the table rows `visible` plus `buffer` rows on each
    /// side, at most `max_rows` rows, visible rows first. Cells handed
    /// out are never handed out again for this result.
    pub fn take_preview_batch(
        &mut self,
        visible: std::ops::Range<usize>,
        buffer: usize,
        max_rows: usize,
    ) -> Option<PreviewBatch> {
        let rowid_col = self.rowid_col?;
        let page_rows = self.page_rows();
        let start = visible.start.min(page_rows);
        let end = visible.end.min(page_rows);
        // Visible rows first, then the buffer below, then above.
        let order = (start..end)
            .chain(end..(end + buffer).min(page_rows))
            .chain((start.saturating_sub(buffer)..start).rev());
        let mut rows: Vec<usize> = Vec::new();
        let mut columns: Vec<usize> = Vec::new();
        for view_row in order {
            if rows.len() >= max_rows {
                break;
            }
            let Some(row) = self.source_row(self.absolute_row(view_row)) else {
                continue;
            };
            let wanted: Vec<usize> = self
                .visible
                .iter()
                .copied()
                .filter(|&col| {
                    !self.previews_asked.contains(&(row, col))
                        && self
                            .source_cell(row, col)
                            .is_some_and(Value::is_unread_clob)
                })
                .collect();
            if wanted.is_empty() || self.source_cell(row, rowid_col).is_none() {
                continue;
            }
            for col in wanted {
                if !columns.contains(&col) {
                    columns.push(col);
                }
            }
            rows.push(row);
        }
        if rows.is_empty() {
            return None;
        }
        let mut rowids = Vec::new();
        for &row in &rows {
            rowids.push(self.source_cell(row, rowid_col)?.display());
            for &col in &columns {
                if self
                    .source_cell(row, col)
                    .is_some_and(Value::is_unread_clob)
                {
                    self.previews_asked.insert((row, col));
                }
            }
        }
        let columns = columns
            .into_iter()
            .map(|col| self.result.columns[col].name.clone())
            .collect();
        Some(PreviewBatch { rowids, columns })
    }

    /// Show CLOB starts read for [`Self::take_preview_batch`]. Only cells
    /// that are still unread take one (an edit or a loaded full value
    /// wins), and only while the previews fit in `room` bytes. Returns how
    /// many cells changed. `room` is what the open results can still hold
    /// (already net of the overrides this grid has).
    pub fn apply_previews(&mut self, previews: Vec<ora_core::ClobPreview>, room: usize) -> usize {
        let Some(rowid_col) = self.rowid_col else {
            return 0;
        };
        let wanted: HashSet<&str> = previews.iter().map(|p| p.rowid.as_str()).collect();
        let mut by_rowid: HashMap<String, usize> = HashMap::new();
        for row in 0..self.result.rows.len() {
            if let Some(Value::Text(id)) = self.source_cell(row, rowid_col) {
                if wanted.contains(id.as_str()) {
                    by_rowid.insert(id.clone(), row);
                }
            }
        }
        let mut used = 0;
        let mut applied = 0;
        for preview in previews {
            let Some(&row) = by_rowid.get(&preview.rowid) else {
                continue;
            };
            let Some(col) = self
                .result
                .columns
                .iter()
                .position(|c| c.name == preview.column)
            else {
                continue;
            };
            let Some(Value::Clipped(clipped)) = self.source_cell(row, col) else {
                continue;
            };
            if !self
                .source_cell(row, col)
                .is_some_and(Value::is_unread_clob)
            {
                continue;
            }
            let value = clipped.with_preview_text(preview.text);
            let size = value.heap_bytes();
            if used + size > room {
                continue;
            }
            used += size;
            self.overrides.insert((row, col), value);
            applied += 1;
        }
        applied
    }

    /// Every field of the row shown at table row `row_ix`, in column
    /// order, without the internal ROWID: the Details panel's content.
    pub fn row_fields(&self, row_ix: usize) -> Vec<(String, Value)> {
        if self.is_insert_row(row_ix) {
            return Vec::new();
        }
        let abs_row = self.absolute_row(row_ix);
        self.result
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name != ROWID_COLUMN)
            .filter_map(|(ix, c)| Some((c.name.clone(), self.cell(abs_row, ix)?.clone())))
            .collect()
    }

    /// Copy, Copy Field Name and the "Copy As" submenu for one row. Values
    /// and names cover the shown columns only (hidden columns and the
    /// ROWID stay out).
    fn copy_menu(
        &self,
        menu: PopupMenu,
        row_ix: usize,
        data_col: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        let abs_row = self.absolute_row(row_ix);
        let fields: Vec<(String, Value)> = self
            .row_fields(row_ix)
            .into_iter()
            .filter(|(name, _)| !self.hidden.contains(name))
            .collect();
        let rowid = self
            .rowid_col
            .and_then(|c| self.cell(abs_row, c))
            .map(Value::display);
        let table = self
            .table_name
            .clone()
            .unwrap_or_else(|| "TABLE_NAME".to_string());
        let cell = self
            .cell(abs_row, data_col)
            .map(ora_core::copy::plain)
            .unwrap_or_default();
        let clipboard = |text: String| {
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }
        };
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let values: Vec<Value> = fields.iter().map(|(_, v)| v.clone()).collect();
        let name_list = names.join(", ");
        let value_list = {
            let refs: Vec<&Value> = values.iter().collect();
            ora_core::copy::join_values(&refs, ", ")
        };
        let menu = menu
            .when(self.row_numbers, |menu| {
                menu.item(PopupMenuItem::new("Copy rows").on_click(clipboard(
                    self.selected_tsv(Some(row_ix)).unwrap_or_default(),
                )))
            })
            .item(PopupMenuItem::new("Copy").on_click(clipboard(value_list)))
            .item(PopupMenuItem::new("Copy Cell").on_click(clipboard(cell)))
            .item(PopupMenuItem::new("Copy Field Name").on_click(clipboard(name_list)));
        let submenu = PopupMenu::build(window, cx, move |sub, _, _| {
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let value_refs: Vec<&Value> = values.iter().collect();
            use ora_core::copy;
            sub.item(PopupMenuItem::new("Insert Statement").on_click(clipboard(
                copy::insert_statement(&table, &name_refs, &value_refs),
            )))
            .item(PopupMenuItem::new("Update Statement").on_click(clipboard(
                copy::update_statement(&table, &name_refs, &value_refs, rowid.as_deref()),
            )))
            .separator()
            .item(
                PopupMenuItem::new("Tab Separated Values (Data only)")
                    .on_click(clipboard(copy::tsv(None, Some(&value_refs)))),
            )
            .item(
                PopupMenuItem::new("Tab Separated Values (Field Name only)")
                    .on_click(clipboard(copy::tsv(Some(&name_refs), None))),
            )
            .item(
                PopupMenuItem::new("Tab Separated Values (Field Name and Data)")
                    .on_click(clipboard(copy::tsv(Some(&name_refs), Some(&value_refs)))),
            )
        });
        menu.item(PopupMenuItem::submenu("Copy As", submenu))
    }

    /// The edit request for the cell at result row `abs_row`, column
    /// `data_col`; `None` when editing is off or the row has no ROWID.
    ///
    /// A clipped cell is not editable: the editor would start from (and
    /// save back) only the kept start. "Load full value" first.
    fn edit_request(&self, abs_row: usize, data_col: usize) -> Option<CellEditRequest> {
        self.on_edit.as_ref()?;
        let rowid = self.cell(abs_row, self.rowid_col?)?.display();
        let value = self.cell(abs_row, data_col)?;
        if value.is_clipped() {
            return None;
        }
        Some(CellEditRequest {
            rowid,
            column: self.result.columns.get(data_col)?.name.clone(),
            value: value.clone(),
        })
    }

    /// "Load full value" for a clipped cell: the item, or a note on how to
    /// see more when the grid has no ROWID to fetch the row by.
    fn load_cell_item(&self, menu: PopupMenu, abs_row: usize, data_col: usize) -> PopupMenu {
        let Some(Value::Clipped(clipped)) = self.cell(abs_row, data_col) else {
            return menu;
        };
        let rowid = self
            .rowid_col
            .and_then(|c| self.cell(abs_row, c))
            .map(Value::display);
        let column = self.result.columns.get(data_col).map(|c| c.name.clone());
        match (self.on_load_cell.clone(), rowid, column) {
            (Some(handler), Some(rowid), Some(column)) => menu
                .item(
                    PopupMenuItem::new(format!("Load full value ({})", clipped.describe_len()))
                        .icon(Icon::new(IconName::Download))
                        .on_click(move |_, window, cx| {
                            handler(rowid.clone(), column.clone(), window, cx)
                        }),
                )
                .separator(),
            _ => menu
                .label(format!(
                    "Value clipped ({}): open the table's data tab to load it",
                    clipped.describe_len()
                ))
                .separator(),
        }
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
        self.clear_cell_selection();
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

    /// Table behind this grid, quoted, for INSERT/UPDATE copies.
    pub fn set_table_name(&mut self, name: Option<String>) {
        self.table_name = name;
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Show `result`. Takes a shared handle: the grid keeps no copy of
    /// the cells (see the module docs).
    pub fn set_result(&mut self, result: impl Into<Arc<QueryResult>>) {
        let result = result.into();
        self.selected_rows.clear();
        self.selection_anchor = 0;
        self.clear_cell_selection();
        self.numeric = numeric_columns(&result);
        self.rowid_col = result.columns.iter().position(|c| c.name == ROWID_COLUMN);
        self.result = result;
        self.order = None;
        self.overrides.clear();
        self.previews_asked.clear();
        self.rebuild_columns();
        self.page = 0;
    }

    /// Result row behind grid row `view_row` (after sorting).
    fn source_row(&self, view_row: usize) -> Option<usize> {
        match &self.order {
            Some(order) => order.get(view_row).copied(),
            None => (view_row < self.result.rows.len()).then_some(view_row),
        }
    }

    /// A cell by result row, with grid edits applied.
    fn source_cell(&self, row: usize, col: usize) -> Option<&Value> {
        self.overrides
            .get(&(row, col))
            .or_else(|| self.result.get(row, col))
    }

    /// The cell at grid row `view_row` (sorted order), result column `col`.
    fn cell(&self, view_row: usize, col: usize) -> Option<&Value> {
        self.source_cell(self.source_row(view_row)?, col)
    }

    pub fn selected_tsv(&self, fallback: Option<usize>) -> Option<String> {
        let row_text = |row: usize| {
            self.source_row(row)?;
            Some(
                self.visible
                    .iter()
                    .map(|&column| {
                        tsv_field(
                            self.cell(row, column)
                                .map(ora_core::copy::plain)
                                .unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\t"),
            )
        };
        if self.selected_rows.is_empty() {
            return fallback
                .filter(|&ix| ix < self.page_rows())
                .and_then(|ix| row_text(self.absolute_row(ix)));
        }
        Some(
            self.selected_rows
                .iter()
                .filter_map(|&ix| row_text(ix))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub fn selected_rowids(&self) -> Vec<String> {
        let Some(column) = self.rowid_col else {
            return Vec::new();
        };
        self.selected_rows
            .iter()
            .filter_map(|row| self.cell(*row, column))
            .map(Value::display)
            .collect()
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn set_page(&mut self, page: usize) {
        self.clear_cell_selection();
        self.page = page.min(self.total_pages().saturating_sub(1));
    }

    pub fn page_rows(&self) -> usize {
        self.total_rows()
            .saturating_sub(self.page * self.page_size)
            .min(self.page_size)
    }

    fn is_insert_row(&self, row: usize) -> bool {
        (self.page_rows()..self.page_rows() + self.insert_rows.len()).contains(&row)
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

    /// Text painted in a grid cell. The cell shows one truncated line, so
    /// a 64 KiB value is cut before layout instead of shaping all of it.
    fn cell_text(&self, row_ix: usize, col_ix: usize) -> String {
        const SHOWN_CHARS: usize = 512;
        let Some(value) = self.cell(row_ix, col_ix) else {
            return String::new();
        };
        match value {
            Value::Text(t) if t.len() > SHOWN_CHARS * 4 => t.chars().take(SHOWN_CHARS).collect(),
            Value::Clipped(c) if !c.preview.display().is_empty() => {
                let preview: String = match &c.preview {
                    Value::Text(t) => t.chars().take(SHOWN_CHARS).collect(),
                    other => other.display(),
                };
                format!("{preview}… ({})", c.describe_len())
            }
            other => other.display(),
        }
    }

    /// Map a visible row index to the underlying result row.
    fn absolute_row(&self, row_ix: usize) -> usize {
        self.page * self.page_size + row_ix
    }
}

/// One field of a spreadsheet-style copy: quoted when it holds a tab,
/// line break or quote, so a paste reads it back as one field.
fn tsv_field(text: String) -> String {
    if text.contains(['\t', '\r', '\n', '"']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
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
            // Compared in place: `display()` would copy both texts for
            // every comparison of a sort.
            (Value::Text(m), Value::Text(n)) => m.cmp(n),
            (Value::Clipped(m), _) => cmp_values(Some(&m.preview), Some(y)),
            (_, Value::Clipped(n)) => cmp_values(Some(x), Some(&n.preview)),
            _ => x.display().cmp(&y.display()),
        },
    }
}

impl TableDelegate for ResultsDelegate {
    fn visible_rows_changed(
        &mut self,
        visible_range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.last_visible = visible_range.clone();
        if let Some(handler) = self.on_visible_rows.clone() {
            handler(visible_range, window, &mut **cx);
        }
    }

    fn render_tr(
        &mut self,
        ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let row = div().id(("row", ix));
        let absolute = self.absolute_row(ix);
        if self.is_insert_row(ix) {
            return row.on_mouse_down(
                MouseButton::Left,
                cx.listener(|table, _, _, cx| {
                    table.delegate_mut().selected_rows.clear();
                    cx.notify();
                }),
            );
        }
        if !self.row_numbers || absolute >= self.total_rows() {
            return row;
        }
        row.when(self.selected_rows.contains(&absolute), |r| {
            r.bg(cx.theme().accent)
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |table, event: &gpui_kit::MouseDownEvent, _, cx| {
                let data = table.delegate_mut();
                if event.modifiers.shift {
                    data.selected_rows.clear();
                    data.selected_rows.extend(
                        data.selection_anchor.min(absolute)..=data.selection_anchor.max(absolute),
                    );
                } else if event.modifiers.control || event.modifiers.platform {
                    if !data.selected_rows.remove(&absolute) {
                        data.selected_rows.insert(absolute);
                    }
                    data.selection_anchor = absolute;
                } else {
                    data.selected_rows.clear();
                    data.selected_rows.insert(absolute);
                    data.selection_anchor = absolute;
                }
                cx.notify();
            }),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Inbox)
                    .size(px(32.))
                    .text_color(crate::theme::empty_hint(cx.theme())),
            )
    }

    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.page_rows() + self.insert_rows.len()
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
        let theme = cx.theme();
        let divider = theme.border;
        div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .when(col_ix > 0, |cell| cell.border_l_1().border_color(divider))
            .pl(px(4.))
            .when(
                self.columns[col_ix].align == gpui_kit::TextAlign::Right,
                |this| this.justify_end(),
            )
            .text_size(crate::theme::ui_size(theme, FONT_GRID))
            .child(self.columns[col_ix].name.clone())
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let pickable = self.row_numbers && !self.is_insert_row(row_ix);
        let abs_row = self.absolute_row(row_ix);
        // Shown-column position of this cell; `None` on the gutter.
        let vis = self
            .data_col(col_ix)
            .map(|_| col_ix - usize::from(self.row_numbers));
        let cell = div()
            .w_full()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, cx| {
                    let data = table.delegate_mut();
                    data.right_col = Some(col_ix);
                    // Right click outside the pick picks that cell, so
                    // the menu's Paste goes where the user clicked.
                    if let (true, Some(vis)) = (pickable, vis) {
                        if !data.cell_picked(abs_row, vis) {
                            data.pick_cell((abs_row, vis), false);
                            cx.notify();
                        }
                    }
                }),
            )
            .when(pickable, |cell| {
                cell.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |table, event: &gpui_kit::MouseDownEvent, _, cx| {
                        let data = table.delegate_mut();
                        match vis {
                            Some(vis) => data.pick_cell((abs_row, vis), event.modifiers.shift),
                            // The gutter picks whole rows, not cells.
                            None => data.clear_cell_selection(),
                        }
                        cx.notify();
                    }),
                )
            })
            .text_size(crate::theme::ui_size(theme, FONT_GRID))
            .line_height(crate::theme::ui_size(theme, LEADING_GRID))
            .truncate();
        if self.is_insert_row(row_ix) {
            let Some(column) = self.data_col(col_ix) else {
                return cell.text_right().text_color(theme.primary).child("+");
            };
            let name = &self.result.columns[column].name;
            return cell.children(
                self.insert_rows[row_ix - self.page_rows()]
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, field)| {
                        Input::new(field)
                            .xsmall()
                            .bordered(false)
                            .disabled(self.insert_disabled)
                    }),
            );
        }
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
        let picked = vis.is_some_and(|vis| pickable && self.cell_picked(abs_row, vis));
        let cell = match (picked, mark) {
            // A picked cell shows the pick; an edit mark stays visible as
            // a stronger tint of its own color.
            (true, Some(CellMark::Staged)) => cell.bg(theme.warning.opacity(0.55)),
            (true, _) => cell.bg(theme.primary.opacity(0.38)),
            (false, Some(CellMark::Staged)) => cell.bg(theme.warning.opacity(0.28)),
            (false, Some(CellMark::Applied)) => cell.bg(theme.primary.opacity(0.22)),
            (false, None) => cell,
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
        let is_null = self.cell(abs_row, data_col).is_some_and(Value::is_null);
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
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        if self.is_insert_row(row_ix) {
            return menu;
        }
        let Some(data_col) = self.right_col.and_then(|c| self.data_col(c)) else {
            return menu;
        };
        let abs_row = self.absolute_row(row_ix);
        menu = self.load_cell_item(menu, abs_row, data_col);
        menu = self.copy_menu(menu, row_ix, data_col, window, &mut **cx);
        menu = menu.separator();
        if let (Some(handler), Some(request)) =
            (self.on_edit.clone(), self.edit_request(abs_row, data_col))
        {
            menu = menu.item(
                PopupMenuItem::new("Edit Cell…").on_click(move |_, window, cx| {
                    handler(request.clone(), window, cx);
                }),
            );
            if let Some(paste) = self.on_cell_paste.clone() {
                let label = match self.picked_cell_count() {
                    0 | 1 => "Paste".to_string(),
                    n => format!("Paste into {n} cells"),
                };
                menu = menu.item(
                    PopupMenuItem::new(label).on_click(move |_, window, cx| paste(window, cx)),
                );
            }
            if let Some(duplicate) = self.on_duplicate_rows.clone() {
                let rows: Vec<_> = self
                    .target_rows(abs_row)
                    .into_iter()
                    .map(|row| self.row_pairs(row))
                    .collect();
                let label = match rows.len() {
                    1 => "Duplicate row".to_string(),
                    n => format!("Duplicate {n} rows"),
                };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .icon(Icon::new(IconName::Copy))
                        .on_click(move |_, window, cx| duplicate(rows.clone(), window, cx)),
                );
            }
            if let Some(delete) = self.on_delete_rows.clone() {
                let rowids = self.delete_targets(abs_row);
                if !rowids.is_empty() {
                    let label = match rowids.len() {
                        1 => "Delete row".to_string(),
                        n => format!("Delete {n} rows"),
                    };
                    menu = menu.separator().item(
                        PopupMenuItem::new(label)
                            .icon(Icon::new(IconName::Trash))
                            .on_click(move |_, window, cx| delete(rowids.clone(), window, cx)),
                    );
                }
            }
            if self.on_quick_filter.is_some() {
                menu = menu.separator();
            }
        }
        let Some(handler) = self.on_quick_filter.clone() else {
            return menu;
        };
        let (Some(column), Some(value)) = (
            self.result.columns.get(data_col),
            self.cell(abs_row, data_col),
        ) else {
            return menu;
        };
        menu = menu.min_w(px(260.)).label("Quick Filter");
        if value.is_clipped() {
            menu = menu.label("Load the full value to filter by its text");
        }
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
        if self.is_insert_row(row_ix) {
            return None;
        }
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
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(col_ix) = self.data_col(col_ix) else {
            return;
        };
        self.selected_rows.clear();
        self.selection_anchor = 0;
        self.clear_cell_selection();
        self.sort_by_column(col_ix, sort);
        // Other rows are on screen now: their CLOB starts are still due.
        if let Some(handler) = self.on_visible_rows.clone() {
            handler(self.last_visible.clone(), window, &mut **cx);
        }
    }
}

impl ResultsDelegate {
    /// Sort the grid rows by result column `col_ix`.
    fn sort_by_column(&mut self, col_ix: usize, sort: ColumnSort) {
        // Sorting orders row indexes; the shared result stays as fetched,
        // so Default restores the fetch order.
        let mut order: Vec<usize> = (0..self.result.rows.len()).collect();
        match sort {
            ColumnSort::Ascending => order.sort_by(|&a, &b| {
                cmp_values(self.source_cell(a, col_ix), self.source_cell(b, col_ix))
            }),
            ColumnSort::Descending => order.sort_by(|&a, &b| {
                cmp_values(self.source_cell(b, col_ix), self.source_cell(a, col_ix))
            }),
            ColumnSort::Default => {
                self.order = None;
                return;
            }
        }
        self.order = Some(order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ora_core::ColumnInfo;

    const MIB: usize = 1024 * 1024;

    fn clob(full_len: u64, preview: &str) -> Value {
        Value::Clipped(Box::new(ora_core::ClippedValue {
            preview: Value::Text(preview.to_string()),
            full_len,
            unit: ora_core::LenUnit::Chars,
        }))
    }

    /// `rows` data rows: ROWID "R<i>", ID and an unread CLOB BODY.
    fn clob_grid(rows: usize) -> ResultsDelegate {
        let col = |name: &str| ColumnInfo {
            name: name.into(),
            data_type: "X".into(),
        };
        let result = QueryResult {
            columns: vec![col(ROWID_COLUMN), col("ID"), col("BODY")],
            rows: (0..rows)
                .map(|i| {
                    vec![
                        Value::Text(format!("R{i}")),
                        Value::Integer(i as i128),
                        clob(500, ""),
                    ]
                })
                .collect(),
            ..Default::default()
        };
        let mut grid = ResultsDelegate::empty()
            .with_page_size(1000)
            .with_row_numbers();
        grid.set_result(result);
        grid
    }

    fn preview(rowid: &str, text: &str) -> ora_core::ClobPreview {
        ora_core::ClobPreview {
            rowid: rowid.into(),
            column: "BODY".into(),
            text: text.into(),
        }
    }

    #[test]
    fn preview_batch_covers_visible_rows_and_buffer_only() {
        let mut grid = clob_grid(1000);
        let batch = grid.take_preview_batch(100..130, 30, 60).unwrap();
        // Visible rows first, then the buffer below, capped at 60 rows.
        assert_eq!(batch.rowids.len(), 60);
        assert_eq!(batch.rowids[0], "R100");
        assert_eq!(batch.rowids[29], "R129");
        assert_eq!(batch.rowids[59], "R159");
        assert_eq!(batch.columns, vec!["BODY".to_string()]);
        // Nothing is asked twice: the same view yields the rows before it.
        let next = grid.take_preview_batch(100..130, 30, 60).unwrap();
        assert_eq!(next.rowids.first().map(String::as_str), Some("R99"));
        assert_eq!(next.rowids.len(), 30);
        assert!(grid.take_preview_batch(100..130, 30, 60).is_none());
        // Rows far from the view were never asked for.
        let far = grid.take_preview_batch(900..910, 0, 60).unwrap();
        assert_eq!(far.rowids.len(), 10);
    }

    #[test]
    fn preview_batch_follows_sort_order_and_clamps_to_the_page() {
        let mut grid = clob_grid(5);
        grid.sort_by_column(1, ColumnSort::Descending);
        let batch = grid.take_preview_batch(0..2, 0, 60).unwrap();
        assert_eq!(batch.rowids, vec!["R4".to_string(), "R3".to_string()]);
        let rest = grid.take_preview_batch(0..50, 50, 60).unwrap();
        assert_eq!(rest.rowids, vec!["R2", "R1", "R0"]);
    }

    #[test]
    fn applied_previews_fill_only_unread_cells_within_room() {
        let mut grid = clob_grid(3);
        let applied = grid.apply_previews(
            vec![
                preview("R0", "hello"),
                preview("R1", &"x".repeat(200)),
                preview("R9", "no such row"),
            ],
            usize::MAX,
        );
        assert_eq!(applied, 2);
        assert_eq!(grid.cell(0, 2), Some(&clob(500, "hello")));
        assert_eq!(grid.cell(2, 2), Some(&clob(500, "")));
        // A cell that is no longer unread (edited, or loaded in full)
        // keeps its value when a late preview arrives.
        grid.set_cell("R2", "BODY", Value::Text("edited".into()));
        assert_eq!(
            grid.apply_previews(vec![preview("R2", "late")], usize::MAX),
            0
        );
        assert_eq!(grid.cell(2, 2), Some(&Value::Text("edited".into())));
        // Read cells are not handed out again, even without a preview.
        assert!(grid.take_preview_batch(0..3, 0, 60).is_none());
        // Previews that do not fit the room are dropped.
        let mut tight = clob_grid(1);
        assert_eq!(tight.apply_previews(vec![preview("R0", "12345678")], 4), 0);
        assert_eq!(tight.cell(0, 2), Some(&clob(500, "")));
    }

    #[test]
    fn a_whole_value_becomes_plain_text_and_a_new_result_starts_over() {
        let mut grid = clob_grid(1);
        grid.apply_previews(vec![preview("R0", &"y".repeat(500))], usize::MAX);
        assert_eq!(grid.cell(0, 2), Some(&Value::Text("y".repeat(500))));
        let mut grid = clob_grid(2);
        assert!(grid.take_preview_batch(0..2, 0, 60).is_some());
        let again = clob_grid(2).result;
        grid.set_result(again);
        assert!(grid.take_preview_batch(0..2, 0, 60).is_some());
    }

    /// The audit's baseline: 512 rows of 32 KiB synthetic text are about
    /// 16 MiB as one `QueryResult`. Before results were shared, holding it
    /// in the statement list, the tab and the grid made that 48 MiB.
    /// Showing (and sorting) it must now only add the view: every holder
    /// points at the same cells, and the grid's own state is a row index.
    #[test]
    fn showing_a_result_does_not_copy_its_cells() {
        let result = QueryResult {
            columns: vec![ColumnInfo {
                name: "BODY".into(),
                data_type: "CLOB".into(),
            }],
            rows: (0..512)
                .map(|i| vec![Value::Text(format!("{i:05}").repeat(32 * 1024 / 5))])
                .collect(),
            ..Default::default()
        };
        assert!((15 * MIB..17 * MIB).contains(&result.heap_bytes()));
        let first_cell = match &result.rows[0][0] {
            Value::Text(t) => t.as_ptr(),
            _ => unreachable!(),
        };

        // The worker's script result, the tab's shown result and the
        // grid: the same path as `Workbench::show_statement_result`.
        let shared = Arc::new(result);
        let statements = [crate::worker::StatementResult {
            sql: "SELECT body FROM doc".into(),
            started: String::new(),
            outcome: Ok(shared.clone()),
        }];
        let last_result = statements[0].outcome.as_ref().ok().cloned();
        let mut grid = ResultsDelegate::empty().with_row_numbers();
        grid.set_result(last_result.clone().unwrap_or_default());
        grid.sort_by_column(0, ColumnSort::Descending);

        // One copy of the cells for all four holders.
        assert!(Arc::ptr_eq(&grid.result, &shared));
        assert_eq!(Arc::strong_count(&shared), 4);
        match &shared.rows[0][0] {
            Value::Text(t) => assert_eq!(t.as_ptr(), first_cell),
            _ => unreachable!(),
        }
        // The grid's extra memory: a row index and no overrides.
        let view = grid
            .order
            .as_ref()
            .map_or(0, |o| o.capacity() * size_of::<usize>());
        assert!(view <= 512 * size_of::<usize>());
        assert!(grid.overrides.is_empty());
        // Sorting did not touch the shared rows.
        assert!(matches!(&shared.rows[0][0], Value::Text(t) if t.starts_with("00000")));
        assert!(grid.cell_text(0, 0).starts_with("00511"));
    }

    #[test]
    fn loaded_full_values_count_as_override_bytes_and_clipped_len_is_known() {
        let clipped = Value::text_limited(&"a".repeat(10_000), 100);
        let result = QueryResult {
            columns: vec![
                ColumnInfo {
                    name: ROWID_COLUMN.into(),
                    data_type: "ROWID".into(),
                },
                ColumnInfo {
                    name: "BODY".into(),
                    data_type: "CLOB".into(),
                },
            ],
            rows: vec![vec![Value::Text("AAA1".into()), clipped]],
            ..Default::default()
        };
        let mut grid = ResultsDelegate::empty();
        grid.set_result(result);
        assert_eq!(grid.override_bytes(), 0);
        assert_eq!(grid.clipped_len("AAA1", "BODY"), Some(10_000));
        assert_eq!(grid.clipped_len("NOPE", "BODY"), None);

        let full = Value::Text("a".repeat(10_000));
        let size = full.heap_bytes();
        grid.set_cell("AAA1", "BODY", full);
        assert_eq!(grid.override_bytes(), size);
        // No longer clipped once loaded.
        assert_eq!(grid.clipped_len("AAA1", "BODY"), None);
        // A new result drops the loaded values (and their bytes).
        grid.set_result(QueryResult::default());
        assert_eq!(grid.override_bytes(), 0);
    }

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
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
        }
    }

    #[test]
    fn copies_selected_visible_cells_as_spreadsheet_rows() {
        let mut result = result_with_rows(3);
        result.columns.push(ColumnInfo {
            name: "Text".into(),
            data_type: "VARCHAR2".into(),
        });
        for row in &mut result.rows {
            row.push(text("a\tb\n\"c\""));
        }
        let mut delegate = ResultsDelegate::empty().with_row_numbers();
        delegate.set_result(result);
        delegate.set_hidden(["N".to_owned()].into_iter().collect());
        delegate.selected_rows.extend([0, 2]);
        let pasted =
            ora_core::import::clipboard_rows(&delegate.selected_tsv(None).unwrap()).unwrap();
        assert_eq!(pasted, vec![vec!["a\tb\n\"c\""]; 2]);
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
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
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

        // Clipped cells (CLOB placeholder, previews) only get the NULL checks.
        let clob = Value::Clipped(Box::new(ora_core::ClippedValue {
            preview: text(""),
            full_len: 415,
            unit: ora_core::LenUnit::Chars,
        }));
        let labels: Vec<String> = quick_filter_entries("TX_DATA", &clob)
            .into_iter()
            .flatten()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(labels, ["TX_DATA IS NULL", "TX_DATA IS NOT NULL"]);
        let preview = Value::Clipped(Box::new(ora_core::ClippedValue {
            preview: text("abc"),
            full_len: 90_000,
            unit: ora_core::LenUnit::Bytes,
        }));
        assert_eq!(quick_filter_entries("T", &preview).len(), 2);

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
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
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
