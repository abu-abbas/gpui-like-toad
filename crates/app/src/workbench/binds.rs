//! Bind variables in the SQL editor, as in Toad: running (or explaining, or
//! dry-running) a statement with `:name` placeholders first asks for a value
//! for each distinct name. The values are written into the SQL as literals
//! ([`ora_core::binds::substitute`]) before the usual run path starts, so
//! the worker, Read Only guard, PROD confirmation, history and the other
//! engines' code paths are untouched. Oracle connections only.
//!
//! The last values of a tab are offered again, and the query history keeps
//! the statement with its `:name` placeholders, never the typed values.

use ora_core::binds::{self, BindKind, BindValue};

use super::ui_helpers::form_label;
use super::*;

/// What to do with the SQL once the values are in.
#[derive(Clone, Copy)]
pub(super) enum BindThen {
    /// F5: selection or whole editor, as a script.
    Run,
    /// Run Current: one statement.
    RunStatement,
    Explain,
    ExplainAnalyze,
    DryRun,
}

struct BindRow {
    key: String,
    name: String,
    input: Entity<InputState>,
    kind: BindKind,
    null: bool,
}

pub(super) struct BindDialog {
    then: BindThen,
    sql: String,
    tab_id: u64,
    rows: Vec<BindRow>,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl Workbench {
    /// True when `sql` has `:name` placeholders to ask about.
    pub(super) fn needs_binds(&self, sql: &str) -> bool {
        self.active_kind() == DbKind::Oracle && !binds::find_binds(sql).is_empty()
    }

    /// Open the dialog for `sql`. The inputs need a window, which the
    /// callers (run actions) do not carry, so it is fetched from the handle.
    pub(super) fn ask_binds(&mut self, sql: String, then: BindThen, cx: &mut Context<Self>) {
        let handle = self.window_handle;
        cx.spawn(async move |view, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.open_bind_dialog(sql, then, window, cx);
                });
            });
        })
        .detach();
    }

    fn open_bind_dialog(
        &mut self,
        sql: String,
        then: BindThen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refs = binds::find_binds(&sql);
        let tab = self.active_tab();
        let tab_id = tab.id;
        let mut rows = Vec::new();
        for name in binds::distinct_names(&refs) {
            let key = binds::bind_key(&name);
            let remembered = tab.bind_values.get(&key);
            let null = remembered.is_some_and(|v| v.value.is_none());
            let last = remembered.cloned().unwrap_or_default();
            let text = last.value.clone().unwrap_or_default();
            let input = cx.new(|cx| {
                InputState::new(&mut *window, cx)
                    .placeholder("value")
                    .default_value(text)
            });
            rows.push(BindRow {
                key,
                name,
                input,
                kind: last.kind,
                null,
            });
        }
        let first = rows.first().map(|r| r.input.clone());
        let subs = rows
            .iter()
            .map(|row| {
                cx.subscribe_in(
                    &row.input,
                    window,
                    |this, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            this.submit_bind_dialog(window, cx);
                        }
                    },
                )
            })
            .collect();
        self.bind_dialog = Some(BindDialog {
            then,
            sql,
            tab_id,
            rows,
            error: None,
            _subs: subs,
        });
        if let Some(input) = first {
            window.defer(cx, move |window, cx| {
                input.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        cx.notify();
    }

    pub(super) fn close_bind_dialog(&mut self, cx: &mut Context<Self>) {
        self.bind_dialog = None;
        cx.notify();
    }

    pub(super) fn submit_bind_dialog(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.bind_dialog.as_mut() else {
            return;
        };
        let mut values = std::collections::HashMap::new();
        for row in &dialog.rows {
            let text = row.input.read(cx).value().to_string();
            values.insert(
                row.key.clone(),
                BindValue {
                    kind: row.kind,
                    value: (!row.null).then_some(text),
                },
            );
        }
        let substituted = match binds::substitute(&dialog.sql, &values) {
            Ok(sql) => sql,
            Err(message) => {
                dialog.error = Some(message);
                cx.notify();
                return;
            }
        };
        let (then, original, tab_id) = (dialog.then, dialog.sql.clone(), dialog.tab_id);
        self.bind_dialog = None;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.bind_values.extend(values);
        }
        if self.active_tab().id != tab_id {
            self.push_message("The tab changed while asking for values; run it again.".into());
            cx.notify();
            return;
        }
        match then {
            BindThen::Run => self.run_resolved(substituted, cx),
            BindThen::RunStatement => self.run_sql_resolved(substituted, cx),
            BindThen::Explain => self.explain_resolved(substituted, cx),
            BindThen::ExplainAnalyze => self.explain_analyze_resolved(substituted, cx),
            BindThen::DryRun => self.open_dryrun_resolved(substituted, cx),
        }
        // History keeps the statement as typed (`:name`), not the values.
        if self.conn.pending_sql.is_some() {
            self.conn.pending_sql = Some(original);
        }
        cx.notify();
    }

    pub(super) fn render_bind_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(dialog) = self.bind_dialog.as_ref() else {
            return div().into_any_element();
        };
        let mut body = div().flex().flex_col().gap(px(SPACE_2));
        body = body.child(
            div()
                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                .text_color(theme.muted_foreground)
                .child(
                    "Enter a value for each bind variable. Strings are quoted for you; a number or \
                     date that does not parse is refused. Tick NULL for no value.",
                ),
        );
        for (ix, row) in dialog.rows.iter().enumerate() {
            let mut kinds = div().flex().flex_row().gap(px(SPACE_1)).flex_none();
            for kind in BindKind::ALL {
                let button = Button::new(("bind-kind", ix * 3 + kind as usize))
                    .small()
                    .label(kind.label());
                let button = if row.kind == kind {
                    button.primary()
                } else {
                    button
                };
                kinds = kinds.child(button.on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(row) = this.bind_dialog.as_mut().and_then(|d| d.rows.get_mut(ix)) {
                        row.kind = kind;
                    }
                    cx.notify();
                })));
            }
            body = body
                .child(form_label(
                    &format!(":{}", row.name.trim_start_matches(':')),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2))
                        .child(Input::new(&row.input).w_full().flex_1())
                        .child(kinds)
                        .child(
                            Checkbox::new(("bind-null", ix))
                                .small()
                                .label("NULL")
                                .checked(row.null)
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    if let Some(row) =
                                        this.bind_dialog.as_mut().and_then(|d| d.rows.get_mut(ix))
                                    {
                                        row.null = *checked;
                                    }
                                    cx.notify();
                                })),
                        ),
                );
        }
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .p(px(SPACE_4))
            .child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                    .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child("Bind variables"),
            )
            .child(body);
        if let Some(error) = &dialog.error {
            content = content.child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        let card = div()
            .id("bind-dialog")
            .flex()
            .flex_col()
            .w(px(620.))
            .max_h(relative(0.9))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_bind_dialog(cx);
                }
            }))
            .child(content)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .bg(theme.muted)
                    .child(
                        Button::new("bind-cancel")
                            .flex_1()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_bind_dialog(cx))),
                    )
                    .child(
                        Button::new("bind-ok")
                            .flex_1()
                            .primary()
                            .label("Run")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_bind_dialog(window, cx)
                            })),
                    ),
            );
        div()
            .id("bind-dialog-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(90.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }
}
