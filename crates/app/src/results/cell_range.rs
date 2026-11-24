//! Cell selection on a data grid, as in Navicat: click a cell to pick it,
//! Shift+click another to pick the rectangle between them. Copy takes the
//! picked cells, and Paste writes the clipboard into them as staged edits:
//! one value fills every picked cell, a block of values (tabs and lines,
//! as spreadsheets copy) is laid out from the top-left picked cell.

use std::ops::RangeInclusive;

use super::{CellEditRequest, ResultsDelegate};

/// A cell of the grid: result row and position among the shown columns.
pub(super) type CellPos = (usize, usize);

/// What a paste into the picked cells would change.
#[derive(Debug, Default, PartialEq)]
pub struct CellPaste {
    /// Each target cell and its new value (`None` is NULL).
    pub edits: Vec<(CellEditRequest, Option<String>)>,
    /// Clipboard values that fell outside the page or the shown columns.
    pub clipped: usize,
}

impl ResultsDelegate {
    /// Rows and shown-column positions of the picked rectangle.
    pub(super) fn cell_rect(&self) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let (r1, c1) = self.cell_anchor?;
        let (r2, c2) = self.cell_focus.unwrap_or((r1, c1));
        Some((r1.min(r2)..=r1.max(r2), c1.min(c2)..=c1.max(c2)))
    }

    pub(super) fn cell_picked(&self, abs_row: usize, vis: usize) -> bool {
        self.cell_rect()
            .is_some_and(|(rows, cols)| rows.contains(&abs_row) && cols.contains(&vis))
    }

    /// Pick one cell, or with `extend` stretch the pick from its anchor.
    pub(super) fn pick_cell(&mut self, cell: CellPos, extend: bool) {
        if extend && self.cell_anchor.is_some() {
            self.cell_focus = Some(cell);
        } else {
            self.cell_anchor = Some(cell);
            self.cell_focus = Some(cell);
        }
    }

    pub fn has_cell_selection(&self) -> bool {
        self.cell_anchor.is_some()
    }

    pub fn clear_cell_selection(&mut self) {
        self.cell_anchor = None;
        self.cell_focus = None;
    }

    /// Number of picked cells, for the status line.
    pub fn picked_cell_count(&self) -> usize {
        self.cell_rect().map_or(0, |(rows, cols)| {
            (rows.end() - rows.start() + 1) * (cols.end() - cols.start() + 1)
        })
    }

    /// The picked cells for the clipboard: one cell as its plain value,
    /// several as tab separated lines that a spreadsheet (or this grid's
    /// own paste) reads back cell for cell.
    pub fn picked_cells_text(&self) -> Option<String> {
        let (rows, cols) = self.cell_rect()?;
        let value = |row: usize, vis: usize| {
            self.visible
                .get(vis)
                .and_then(|&col| self.cell(row, col))
                .map(ora_core::copy::plain)
                .unwrap_or_default()
        };
        if rows.start() == rows.end() && cols.start() == cols.end() {
            return Some(value(*rows.start(), *cols.start()));
        }
        Some(
            rows.map(|row| {
                cols.clone()
                    .map(|vis| super::tsv_field(value(row, vis)))
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        )
    }

    /// The staged edits a paste of `values` (clipboard rows of fields)
    /// into the picked cells makes. Empty values become NULL, the way
    /// Oracle stores an empty string. Cells the paste would not change are
    /// left out; rows past the current page and columns past the last
    /// shown one are not written and are counted in `clipped`.
    pub fn cell_paste(&self, values: &[Vec<String>]) -> CellPaste {
        let mut out = CellPaste::default();
        let Some((rows, cols)) = self.cell_rect() else {
            return out;
        };
        let page_end = self.absolute_row(self.page_rows());
        let new_value = |text: &str| (!text.is_empty()).then(|| text.to_string());
        let push = |row: usize, vis: usize, text: &str, out: &mut CellPaste| {
            if row >= page_end || vis >= self.visible.len() {
                out.clipped += 1;
                return;
            }
            let Some(request) = self.edit_request(row, self.visible[vis]) else {
                return;
            };
            let new = new_value(text);
            let same = match &new {
                None => request.value.is_null(),
                Some(t) => !request.value.is_null() && request.value.display() == *t,
            };
            let pending = self
                .pending
                .contains_key(&(request.rowid.clone(), request.column.clone()));
            if !same || pending {
                out.edits.push((request, new));
            }
        };
        match values {
            [] => {}
            // One value: fill every picked cell with it.
            [row] if row.len() == 1 => {
                for r in rows.clone() {
                    for c in cols.clone() {
                        push(r, c, &row[0], &mut out);
                    }
                }
            }
            // A block: lay it out from the top-left picked cell.
            block => {
                for (dr, fields) in block.iter().enumerate() {
                    for (dc, text) in fields.iter().enumerate() {
                        push(rows.start() + dr, cols.start() + dc, text, &mut out);
                    }
                }
            }
        }
        out
    }

    /// After a block paste, pick the area it covered so the user sees it.
    pub fn pick_pasted_block(&mut self, values: &[Vec<String>]) {
        let Some((rows, cols)) = self.cell_rect() else {
            return;
        };
        let height = values.len();
        let width = values.iter().map(Vec::len).max().unwrap_or(0);
        if height == 0 || width == 0 || (height == 1 && width == 1) {
            return;
        }
        let last_row = self.absolute_row(self.page_rows()).saturating_sub(1);
        let last_col = self.visible.len().saturating_sub(1);
        self.cell_anchor = Some((*rows.start(), *cols.start()));
        self.cell_focus = Some((
            (rows.start() + height - 1).min(last_row),
            (cols.start() + width - 1).min(last_col),
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use ora_core::{ColumnInfo, QueryResult, Value, ROWID_COLUMN};

    use super::*;

    fn grid(rows: usize) -> ResultsDelegate {
        let column = |name: &str| ColumnInfo {
            name: name.into(),
            data_type: "VARCHAR2".into(),
        };
        let result = QueryResult {
            columns: vec![column("A"), column(ROWID_COLUMN), column("B"), column("C")],
            rows: (0..rows)
                .map(|i| {
                    vec![
                        Value::Text(format!("a{i}")),
                        Value::Text(format!("R{i}")),
                        Value::Text(format!("b{i}")),
                        Value::Null,
                    ]
                })
                .collect(),
            truncated: false,
            byte_limited: false,
            elapsed: Default::default(),
            fetch_time: None,
            order_by: Vec::new(),
        };
        let mut delegate = ResultsDelegate::empty()
            .with_row_numbers()
            .with_cell_edit(Rc::new(|_, _, _| {}));
        delegate.set_result(result);
        delegate
    }

    fn targets(paste: &CellPaste) -> Vec<(String, String, Option<String>)> {
        paste
            .edits
            .iter()
            .map(|(r, v)| (r.rowid.clone(), r.column.clone(), v.clone()))
            .collect()
    }

    fn one(text: &str) -> Vec<Vec<String>> {
        vec![vec![text.to_string()]]
    }

    #[test]
    fn single_value_fills_every_picked_cell() {
        let mut g = grid(3);
        g.pick_cell((0, 0), false);
        g.pick_cell((1, 1), true);
        assert_eq!(g.picked_cell_count(), 4);
        let paste = g.cell_paste(&one("x"));
        assert_eq!(
            targets(&paste),
            vec![
                ("R0".into(), "A".into(), Some("x".into())),
                ("R0".into(), "B".into(), Some("x".into())),
                ("R1".into(), "A".into(), Some("x".into())),
                ("R1".into(), "B".into(), Some("x".into())),
            ]
        );
        assert_eq!(paste.clipped, 0);
    }

    #[test]
    fn block_is_laid_out_from_top_left_and_clipped_at_the_edges() {
        let mut g = grid(2);
        // Pick from the bottom-right so the anchor is not the top-left.
        g.pick_cell((1, 2), false);
        g.pick_cell((1, 1), true);
        let values = vec![
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
            vec!["s".to_string(), "t".to_string(), "u".to_string()],
        ];
        let paste = g.cell_paste(&values);
        assert_eq!(
            targets(&paste),
            vec![
                ("R1".into(), "B".into(), Some("p".into())),
                ("R1".into(), "C".into(), Some("q".into())),
            ]
        );
        // "r" falls past the last column; the second line past the last row.
        assert_eq!(paste.clipped, 4);
        g.pick_pasted_block(&values);
        assert_eq!(g.cell_rect(), Some((1..=1, 1..=2)));
    }

    #[test]
    fn empty_value_is_null_and_unchanged_cells_are_skipped() {
        let mut g = grid(2);
        g.pick_cell((0, 1), false);
        g.pick_cell((1, 2), true);
        let paste = g.cell_paste(&one(""));
        // C is already NULL; only B changes.
        assert_eq!(
            targets(&paste),
            vec![
                ("R0".into(), "B".into(), None),
                ("R1".into(), "B".into(), None),
            ]
        );
        g.pick_cell((0, 0), false);
        assert!(g.cell_paste(&one("a0")).edits.is_empty());
    }

    #[test]
    fn grid_without_rowid_or_edit_handler_takes_no_paste() {
        let mut g = grid(1);
        g.on_edit = None;
        g.pick_cell((0, 0), false);
        assert!(g.cell_paste(&one("x")).edits.is_empty());
    }

    #[test]
    fn picked_cells_copy_as_value_or_tsv_block() {
        let mut g = grid(2);
        g.pick_cell((1, 0), false);
        assert_eq!(g.picked_cells_text().as_deref(), Some("a1"));
        g.pick_cell((0, 2), true);
        let text = g.picked_cells_text().unwrap();
        assert_eq!(text, "a0\tb0\t\na1\tb1\t");
        assert_eq!(
            ora_core::import::clipboard_rows(&text).unwrap(),
            vec![vec!["a0", "b0", ""], vec!["a1", "b1", ""]]
        );
    }

    #[test]
    fn new_result_or_hidden_columns_drop_the_pick() {
        let mut g = grid(2);
        g.pick_cell((0, 0), false);
        g.set_hidden(["A".to_owned()].into_iter().collect());
        assert!(!g.has_cell_selection());
        g.pick_cell((0, 0), false);
        g.set_result(QueryResult::default());
        assert!(!g.has_cell_selection());
    }

    #[test]
    fn tab_and_enter_walk_the_editable_cells() {
        let g = grid(2);
        let at = |from: (usize, usize), key| g.neighbor_request(from, key).map(|r| r.pos);
        // Columns shown: A (0), B (2), C (3); the ROWID column is hidden.
        assert_eq!(at((0, 0), crate::results::CellKey::Next), Some((0, 2)));
        assert_eq!(at((0, 3), crate::results::CellKey::Next), Some((1, 0)));
        assert_eq!(at((1, 3), crate::results::CellKey::Next), None);
        assert_eq!(at((1, 0), crate::results::CellKey::Prev), Some((0, 3)));
        assert_eq!(at((0, 0), crate::results::CellKey::Prev), None);
        assert_eq!(at((0, 2), crate::results::CellKey::Done), Some((1, 2)));
        assert_eq!(at((1, 2), crate::results::CellKey::Done), None);
        assert_eq!(at((1, 2), crate::results::CellKey::Up), Some((0, 2)));
        assert_eq!(at((0, 2), crate::results::CellKey::Up), None);
    }
}
