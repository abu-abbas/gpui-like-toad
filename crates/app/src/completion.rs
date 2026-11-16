//! Glue between the editor's completion popup and the UI-free engine in
//! [`ora_core::completion`].
//!
//! gpui-kit's editor already owns the popup, the caret anchoring and the
//! arrow/Enter/Esc handling; all it needs is a [`CompletionProvider`]. The
//! provider answers from an in-memory [`Catalog`] shared by every query
//! tab, so a keystroke never waits on the database. The workbench fills the
//! catalog from the DB worker thread.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::{App, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};
use ora_core::completion::{complete, Catalog, SuggestionKind};

/// Called when the user typed `SCHEMA.` for a schema not loaded yet.
pub type MissingSchemaHandler = Rc<dyn Fn(String, &mut App)>;

pub struct SqlCompletionProvider {
    catalog: Rc<RefCell<Catalog>>,
    on_missing_schema: MissingSchemaHandler,
}

impl SqlCompletionProvider {
    pub fn new(catalog: Rc<RefCell<Catalog>>, on_missing_schema: MissingSchemaHandler) -> Self {
        SqlCompletionProvider {
            catalog,
            on_missing_schema,
        }
    }
}

fn item_kind(kind: SuggestionKind) -> CompletionItemKind {
    match kind {
        SuggestionKind::Keyword => CompletionItemKind::KEYWORD,
        SuggestionKind::Function => CompletionItemKind::FUNCTION,
        SuggestionKind::Schema => CompletionItemKind::MODULE,
        SuggestionKind::Table => CompletionItemKind::CLASS,
        SuggestionKind::View => CompletionItemKind::INTERFACE,
        SuggestionKind::Column => CompletionItemKind::FIELD,
        SuggestionKind::Alias => CompletionItemKind::VARIABLE,
        SuggestionKind::Object => CompletionItemKind::REFERENCE,
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        // Computed inline: the catalog is in memory and matching a few
        // thousand names takes well under a frame.
        let source = text.to_string();
        let result = complete(&source, offset, &self.catalog.borrow());
        if let Some(schema) = result.missing_schema {
            // Deferred: this runs inside an editor update, and the handler
            // updates the workbench.
            let handler = self.on_missing_schema.clone();
            cx.defer(move |cx| handler(schema, cx));
        }
        let range = Range {
            start: text.offset_to_position(result.replace.start),
            end: text.offset_to_position(result.replace.end),
        };
        let items = result
            .items
            .into_iter()
            .enumerate()
            .map(|(ix, suggestion)| CompletionItem {
                label: suggestion.label,
                kind: Some(item_kind(suggestion.kind)),
                detail: Some(suggestion.detail),
                // Keep the engine's ranking whatever the menu sorts by.
                sort_text: Some(format!("{ix:04}")),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: suggestion.insert,
                })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// Open (or refine) the popup while an identifier is being typed and
    /// right after a dot. Whitespace, punctuation and pastes do not, so the
    /// popup never appears on its own between words.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        let mut chars = new_text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c == '.' || c == '_' || c.is_alphanumeric(),
            _ => false,
        }
    }
}
