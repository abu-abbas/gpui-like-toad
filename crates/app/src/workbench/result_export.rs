//! "Export" of a SQL result: Save As dialog, then the file is written on a
//! background thread (never the UI thread) with a progress readout and a
//! Cancel button. The file appears only when the export finished
//! (see `ora_core::export::write_file_atomically`), so a cancel or an error
//! leaves no half-written file. What is written is the result already in
//! memory, so it respects the byte limits of the fetch (32 MiB per result,
//! long cells clipped); the message says when the file is shorter than the
//! query.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ora_core::export::{CsvMode, ExportError};

use super::*;

/// Which file a result export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResultExportKind {
    Csv,
    CsvSpreadsheetSafe,
    Xlsx,
}

impl ResultExportKind {
    fn extension(self) -> &'static str {
        match self {
            ResultExportKind::Csv | ResultExportKind::CsvSpreadsheetSafe => "csv",
            ResultExportKind::Xlsx => "xlsx",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ResultExportKind::Csv => "CSV",
            ResultExportKind::CsvSpreadsheetSafe => "CSV (spreadsheet-safe)",
            ResultExportKind::Xlsx => "XLSX",
        }
    }
}

/// An export in progress, shown in the results bar instead of the Export
/// menu. One at a time.
pub(super) struct ResultExport {
    kind: ResultExportKind,
    cancel: Arc<AtomicBool>,
    rows_done: Arc<AtomicUsize>,
    total_rows: usize,
}

impl ResultExport {
    /// "CSV 42%": rows written so far over the rows to write.
    pub(super) fn progress_text(&self) -> String {
        format!(
            "{} {}%",
            self.kind.label(),
            percent(self.rows_done.load(Ordering::Relaxed), self.total_rows)
        )
    }

    pub(super) fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn percent(done: usize, total: usize) -> usize {
    (done.min(total) * 100).checked_div(total).unwrap_or(100)
}

/// Export file name suggested in the Save As dialog.
fn suggested_name(stamp: u64, kind: ResultExportKind) -> String {
    format!("Kilat-export-{stamp}.{}", kind.extension())
}

/// The message after an export ends. `note` is the result's limit note
/// (empty when the file holds the whole query result).
fn finished_message(
    kind: ResultExportKind,
    path: &std::path::Path,
    rows: usize,
    outcome: &Result<(), ExportError>,
    note: &str,
) -> String {
    match outcome {
        Ok(()) => format!(
            "{} saved: {} ({rows} rows){note}",
            kind.label(),
            path.display()
        ),
        Err(ExportError::Cancelled) => {
            format!("{} export cancelled; no file was written.", kind.label())
        }
        Err(ExportError::Failed(message)) => {
            format!("Failed to write {}: {message}", kind.label())
        }
    }
}

fn write_result(
    kind: ResultExportKind,
    result: &QueryResult,
    path: &std::path::Path,
    progress: &mut dyn FnMut(usize) -> bool,
) -> Result<(), ExportError> {
    use ora_core::export::{write_csv_file, write_xlsx_file};
    match kind {
        ResultExportKind::Csv => write_csv_file(result, CsvMode::Raw, path, progress),
        ResultExportKind::CsvSpreadsheetSafe => {
            write_csv_file(result, CsvMode::SpreadsheetSafe, path, progress)
        }
        ResultExportKind::Xlsx => write_xlsx_file(result, path, progress),
    }
}

impl Workbench {
    pub(super) fn export_result(&mut self, kind: ResultExportKind, cx: &mut Context<Self>) {
        if self.result_export.is_some() {
            self.push_message("An export is already running.".to_string());
            cx.notify();
            return;
        }
        let Some(result) = self.active_tab().last_result.clone() else {
            self.push_message("There are no query results to export.".to_string());
            self.active_tab_mut().results_tab = ResultsTab::Messages;
            cx.notify();
            return;
        };
        let suggested = suggested_name(unix_stamp(), kind);
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |view, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return, // cancelled
                // No file dialog on this system: save in the working directory.
                _ => dir.join(&suggested),
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let rows_done = Arc::new(AtomicUsize::new(0));
            let outcome: Arc<Mutex<Option<Result<(), ExportError>>>> = Arc::default();
            let started = view.update(cx, |this, cx| {
                this.result_export = Some(ResultExport {
                    kind,
                    cancel: cancel.clone(),
                    rows_done: rows_done.clone(),
                    total_rows: result.row_count(),
                });
                this.conn.status = format!("Exporting {}…", kind.label());
                cx.notify();
            });
            if started.is_err() {
                return;
            }
            cx.background_executor()
                .spawn({
                    let (result, path) = (result.clone(), path.clone());
                    let (cancel, rows_done, outcome) =
                        (cancel.clone(), rows_done.clone(), outcome.clone());
                    async move {
                        let done = write_result(kind, &result, &path, &mut |n| {
                            rows_done.store(n, Ordering::Relaxed);
                            !cancel.load(Ordering::Relaxed)
                        });
                        if let Ok(mut slot) = outcome.lock() {
                            *slot = Some(done);
                        }
                    }
                })
                .detach();
            // Redraw for the progress readout until the writer reports back.
            let outcome = loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let finished = outcome.lock().ok().and_then(|mut slot| slot.take());
                if let Some(done) = finished {
                    break done;
                }
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            };
            let _ = view.update(cx, |this, cx| {
                this.result_export = None;
                let message = finished_message(
                    kind,
                    &path,
                    result.row_count(),
                    &outcome,
                    &results::limit_note(&result),
                );
                this.conn.status = message.clone();
                this.push_message(message);
                this.active_tab_mut().results_tab = ResultsTab::Messages;
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_bounded() {
        assert_eq!(percent(0, 0), 100);
        assert_eq!(percent(0, 200), 0);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(999, 200), 100);
    }

    #[test]
    fn names_and_messages() {
        assert_eq!(
            suggested_name(5, ResultExportKind::CsvSpreadsheetSafe),
            "Kilat-export-5.csv"
        );
        assert_eq!(
            suggested_name(5, ResultExportKind::Xlsx),
            "Kilat-export-5.xlsx"
        );
        let path = std::path::Path::new("/tmp/a.csv");
        let kind = ResultExportKind::Csv;
        assert_eq!(
            finished_message(kind, path, 3, &Ok(()), ""),
            "CSV saved: /tmp/a.csv (3 rows)"
        );
        assert!(
            finished_message(kind, path, 3, &Err(ExportError::Cancelled), "")
                .contains("no file was written")
        );
        assert!(finished_message(
            kind,
            path,
            3,
            &Err(ExportError::Failed("disk full".into())),
            ""
        )
        .starts_with("Failed to write CSV: disk full"));
    }

    #[test]
    fn written_file_matches_the_chosen_mode() {
        let result = QueryResult {
            columns: vec![ora_core::ColumnInfo {
                name: "A".into(),
                data_type: "VARCHAR2".into(),
            }],
            rows: vec![vec![ora_core::Value::Text("=1+1".into())]],
            ..QueryResult::default()
        };
        // Test-only scratch dir, unique per process; no secrets involved.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("kilat-result-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.csv");
        write_result(ResultExportKind::Csv, &result, &path, &mut |_| true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\r\n=1+1\r\n");
        write_result(
            ResultExportKind::CsvSpreadsheetSafe,
            &result,
            &path,
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\r\n'=1+1\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
