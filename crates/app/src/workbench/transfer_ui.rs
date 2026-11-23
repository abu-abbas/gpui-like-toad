//! Data Transfer wizard: the modal and its steps (Source/Target, Database
//! Objects, Run), plus the Option, field picker, Recordset Generator,
//! profile and confirmation panels.

use std::rc::Rc;

use ora_core::transfer::ranges::RangeKind;
use ora_core::transfer::{LogLevel, NameCase};

use super::transfer::{OpenConn, Panel, Step, TablesState, TargetMode};
use super::*;

type Pick = Rc<dyn Fn(&mut Workbench, &mut Context<Workbench>)>;

fn section(text: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(theme, FONT_BODY))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.primary)
        .child(text.to_string())
}

fn muted(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    caption_text(text, theme)
}

impl Workbench {
    /// A select-style button with a menu of `(label, checked, action)`.
    fn transfer_dropdown(
        &self,
        id: &'static str,
        label: String,
        items: Vec<(String, bool, Pick)>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        Button::new(id)
            .small()
            .ghost()
            .bg(theme.secondary)
            .h(px(26.))
            .w_full()
            .child(select_trigger(label, theme))
            .disabled(items.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(320.)).min_w(px(220.));
                for (label, checked, action) in &items {
                    let (view, action) = (view.clone(), action.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(*checked)
                            .on_click(move |_, _, cx: &mut App| {
                                let action = action.clone();
                                view.update(cx, |this, cx| action(this, cx));
                            }),
                    );
                }
                menu
            })
    }

    fn conn_items(&self, current: Option<u64>, source_side: bool) -> Vec<(String, bool, Pick)> {
        self.open_conns()
            .into_iter()
            .map(|c| {
                let id = c.id;
                let action: Pick = Rc::new(move |this, cx| {
                    if source_side {
                        this.transfer_set_source(id, cx)
                    } else {
                        this.transfer_set_target(id, cx)
                    }
                });
                (
                    format!("{} ({})", c.label, c.kind.label()),
                    current == Some(id),
                    action,
                )
            })
            .collect()
    }

    fn schema_items(
        schemas: &[String],
        current: Option<&String>,
        source_side: bool,
    ) -> Vec<(String, bool, Pick)> {
        schemas
            .iter()
            .map(|s| {
                let pick = s.clone();
                let action: Pick = Rc::new(move |this, cx| {
                    if source_side {
                        this.transfer_set_source_schema(pick.clone(), cx)
                    } else {
                        this.transfer_set_target_schema(pick.clone(), cx)
                    }
                });
                (s.clone(), current == Some(s), action)
            })
            .collect()
    }

    pub(super) fn render_transfer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(w) = self.transfer.as_ref() else {
            return div().into_any_element();
        };
        let running = w.run.as_ref().is_some_and(|r| r.running);
        let label_of = |id: Option<u64>| -> String {
            id.and_then(|id| self.open_conn(id))
                .map_or("—".to_string(), |c| c.label)
        };
        let target_label = match w.target_mode {
            TargetMode::Connection => format!(
                "{} · {}",
                label_of(w.target),
                w.target_schema.clone().unwrap_or_default()
            ),
            TargetMode::File if w.file_csv => "CSV files".to_string(),
            TargetMode::File => "SQL file".to_string(),
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child("Data Transfer"),
            )
            .child(muted(
                format!(
                    "{} · {}   →   {}",
                    label_of(w.source),
                    w.source_schema.clone().unwrap_or_default(),
                    target_label
                ),
                theme,
            ));

        let body = match w.panel {
            Panel::Options => self.render_transfer_options(theme, cx).into_any_element(),
            Panel::Fields => self.render_transfer_fields(theme, cx).into_any_element(),
            Panel::Generator => self.render_transfer_generator(theme, cx).into_any_element(),
            Panel::SaveProfile => self.render_transfer_save(theme, cx).into_any_element(),
            Panel::LoadProfile => self.render_transfer_load(theme, cx).into_any_element(),
            Panel::Confirm => self.render_transfer_confirm(theme, cx).into_any_element(),
            Panel::None => match w.step {
                Step::Endpoints => self.render_transfer_endpoints(theme, cx).into_any_element(),
                Step::Objects => self.render_transfer_objects(theme, cx).into_any_element(),
                Step::Run => self.render_transfer_run(theme, cx).into_any_element(),
            },
        };

        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .bg(theme.muted);
        if w.panel == Panel::None {
            let can_edit = !running;
            footer = footer
                .child(
                    Button::new("tr-save")
                        .small()
                        .label("Save Profile")
                        .disabled(!can_edit || w.step == Step::Endpoints)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.transfer_set_panel(Panel::SaveProfile, cx)
                        })),
                )
                .child(
                    Button::new("tr-load")
                        .small()
                        .label("Load Profile")
                        .disabled(!can_edit)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.transfer_open_load_profile(cx)),
                        ),
                )
                .child(
                    Button::new("tr-option")
                        .small()
                        .label("Option")
                        .disabled(!can_edit)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.transfer_set_panel(Panel::Options, cx)
                        })),
                )
                .child(div().flex_1());
            if w.step != Step::Endpoints {
                footer = footer.child(
                    Button::new("tr-prev")
                        .small()
                        .label("Previous")
                        .disabled(running)
                        .on_click(cx.listener(|this, _, _, cx| this.transfer_back(cx))),
                );
            }
            footer = match w.step {
                Step::Run => match &w.run {
                    Some(run) if run.running => footer.child(
                        Button::new("tr-stop")
                            .small()
                            .danger()
                            .label("Stop")
                            .on_click(cx.listener(|this, _, _, cx| this.transfer_stop(cx))),
                    ),
                    Some(_) => footer.child(
                        Button::new("tr-close")
                            .small()
                            .primary()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_data_transfer(cx))),
                    ),
                    None => {
                        footer
                            .child(Button::new("tr-cancel").small().label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| this.close_data_transfer(cx)),
                            ))
                            .child(
                                Button::new("tr-start")
                                    .small()
                                    .primary()
                                    .label("Start")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.transfer_start(window, cx)
                                    })),
                            )
                    }
                },
                _ => footer
                    .child(
                        Button::new("tr-cancel")
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_data_transfer(cx))),
                    )
                    .child(
                        Button::new("tr-next")
                            .small()
                            .primary()
                            .label("Next")
                            .on_click(cx.listener(|this, _, _, cx| this.transfer_next(cx))),
                    ),
            };
        }
        let error = w.error.clone().map(|e| {
            div()
                .px(px(SPACE_4))
                .py(px(SPACE_2))
                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                .text_color(theme.danger)
                .child(e)
        });

        let card = div()
            .id("transfer-dialog")
            .flex()
            .flex_col()
            .w(px(900.))
            .h(px(620.))
            .max_w(relative(0.96))
            .max_h(relative(0.94))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    let panel_open = this
                        .transfer
                        .as_ref()
                        .is_some_and(|w| w.panel != Panel::None);
                    if panel_open {
                        this.transfer_set_panel(Panel::None, cx);
                    } else {
                        this.close_data_transfer(cx);
                    }
                }
            }))
            .child(header)
            .child(
                div()
                    .id("transfer-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(SPACE_4))
                    .child(body),
            )
            .children(error)
            .when(w.panel == Panel::None, |card| card.child(footer));
        div()
            .id("transfer-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }

    fn info_block(
        &self,
        conn: Option<OpenConn>,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(section("Information", theme));
        match conn {
            Some(c) => {
                for (k, v) in c.info {
                    block = block.child(body_text(format!("{k}: {v}"), theme));
                }
            }
            None => block = block.child(muted("No connection selected.".into(), theme)),
        }
        block
    }

    fn render_transfer_endpoints(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let source_label = w
            .source
            .and_then(|id| self.open_conn(id))
            .map_or("Choose…".to_string(), |c| {
                format!("{} ({})", c.label, c.kind.label())
            });
        let source = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(px(SPACE_2))
            .child(section("Source", theme))
            .child(form_label("Connection", theme))
            .child(self.transfer_dropdown(
                "tr-src-conn",
                source_label,
                self.conn_items(w.source, true),
                theme,
                cx,
            ))
            .child(form_label("Schema", theme))
            .child(self.transfer_dropdown(
                "tr-src-schema",
                w.source_schema.clone().unwrap_or_else(|| "Choose…".into()),
                Self::schema_items(&w.source_schemas, w.source_schema.as_ref(), true),
                theme,
                cx,
            ))
            .child(div().h(px(SPACE_4)))
            .child(self.info_block(w.source.and_then(|id| self.open_conn(id)), theme));

        let mode = |id: &'static str, label: &'static str, m: TargetMode| {
            Button::new(id)
                .small()
                .label(label)
                .selected(w.target_mode == m)
                .on_click(cx.listener(move |this, _, _, cx| this.transfer_set_mode(m, cx)))
        };
        let mut target = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .gap(px(SPACE_2))
            .child(section("Target", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(mode("tr-mode-conn", "Connection", TargetMode::Connection))
                    .child(mode("tr-mode-file", "File", TargetMode::File)),
            );
        match w.target_mode {
            TargetMode::Connection => {
                let label = w
                    .target
                    .and_then(|id| self.open_conn(id))
                    .map_or("Choose…".to_string(), |c| {
                        format!("{} ({})", c.label, c.kind.label())
                    });
                target = target
                    .child(form_label("Connection", theme))
                    .child(self.transfer_dropdown(
                        "tr-dst-conn",
                        label,
                        self.conn_items(w.target, false),
                        theme,
                        cx,
                    ))
                    .child(form_label("Schema", theme))
                    .child(self.transfer_dropdown(
                        "tr-dst-schema",
                        w.target_schema.clone().unwrap_or_else(|| "Choose…".into()),
                        Self::schema_items(&w.target_schemas, w.target_schema.as_ref(), false),
                        theme,
                        cx,
                    ))
                    .child(div().h(px(SPACE_4)))
                    .child(self.info_block(w.target.and_then(|id| self.open_conn(id)), theme));
            }
            TargetMode::File => {
                let kind_button = |kind: DbKind| {
                    Button::new(("tr-sk", kind as usize))
                        .small()
                        .label(kind.label())
                        .selected(w.script_kind == kind)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(w) = this.transfer.as_mut() {
                                w.script_kind = kind;
                            }
                            cx.notify();
                        }))
                };
                let format_button = |id: &'static str, label: &'static str, csv: bool| {
                    Button::new(id)
                        .small()
                        .label(label)
                        .selected(w.file_csv == csv)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(w) = this.transfer.as_mut() {
                                w.file_csv = csv;
                                w.file_from_dialog = false;
                            }
                            cx.notify();
                        }))
                };
                target = target.child(form_label("Format", theme)).child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(SPACE_2))
                        .child(format_button("tr-ff-sql", "SQL script", false))
                        .child(format_button("tr-ff-csv", "CSV files", true)),
                );
                if w.file_csv {
                    target = target
                        .child(form_label("Folder (one .csv per table)", theme))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(SPACE_2))
                                .child(Input::new(&w.file_input).flex_1())
                                .child(Button::new("tr-browse").small().label("Browse…").on_click(
                                    cx.listener(|this, _, _, cx| this.transfer_browse_file(cx)),
                                )),
                        )
                        .child(muted(
                            "Existing files are not overwritten unless Drop target objects before create is on. Keys, indexes and triggers do not apply to CSV.".into(),
                            theme,
                        ));
                } else {
                    target = target
                    .child(form_label("SQL file", theme))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(SPACE_2))
                            .child(Input::new(&w.file_input).flex_1())
                            .child(Button::new("tr-browse").small().label("Browse…").on_click(
                                cx.listener(|this, _, _, cx| this.transfer_browse_file(cx)),
                            )),
                    )
                    .child(form_label("Write the script for", theme))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(SPACE_2))
                            .children(
                                DbKind::ALL
                                    .into_iter()
                                    .filter(|k| {
                                        *k == DbKind::Oracle || self.extensions.db_enabled(*k)
                                    })
                                    .map(kind_button),
                            ),
                    )
                    .child(form_label("Schema name in the script", theme))
                    .child(Input::new(&w.file_schema).w_full())
                    .child(muted(
                        "The script holds CREATE TABLE and INSERT statements; nothing is sent to a database."
                            .into(),
                        theme,
                    ));
                }
            }
        }
        let swap = div().flex().flex_none().items_start().pt(px(70.)).child(
            Button::new("tr-swap")
                .ghost()
                .size(px(30.))
                .icon(icon(IconName::ArrowLeftRight, 18., theme.muted_foreground))
                .tooltip("Swap source and target")
                .disabled(w.target_mode != TargetMode::Connection || w.target.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.transfer_swap(cx))),
        );
        div()
            .flex()
            .flex_row()
            .gap(px(SPACE_4))
            .size_full()
            .child(source)
            .child(swap)
            .child(target)
    }

    fn render_transfer_objects(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let needle = w.search.read(cx).value().to_lowercase();
        let shown: Vec<String> = w
            .tables
            .iter()
            .filter(|t| needle.is_empty() || t.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        let all_on = !shown.is_empty() && shown.iter().all(|t| w.selected.contains(t));
        let toggle_tables = shown.clone();
        let mut list = div()
            .id("tr-table-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .border_1()
            .border_color(theme.border)
            .rounded(px(RADIUS_SM));
        match &w.tables_state {
            TablesState::Loading => {
                list = list.child(
                    div()
                        .p(px(SPACE_3))
                        .child(loading_state("Loading tables…", theme)),
                )
            }
            TablesState::Failed(e) => {
                list = list.child(
                    div()
                        .p(px(SPACE_3))
                        .child(muted(format!("Could not load tables: {e}"), theme)),
                )
            }
            _ if shown.is_empty() => {
                list = list.child(
                    div()
                        .p(px(SPACE_3))
                        .child(empty_state_text("No tables".into(), theme)),
                )
            }
            _ => {}
        }
        for (ix, table) in shown.iter().enumerate() {
            let on = w.selected.contains(table);
            let focused = w.focus.as_ref() == Some(table);
            let (check_name, name) = (table.clone(), table.clone());
            list = list.child(
                div()
                    .id(("tr-row", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_2))
                    .h(px(26.))
                    .flex_none()
                    .when(focused, |d| d.bg(theme.accent))
                    .child(
                        Checkbox::new(("tr-check", ix))
                            .small()
                            .checked(on)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.transfer_toggle_table(&check_name, *checked, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .text_color(theme.foreground)
                            .child(table.clone()),
                    )
                    .when(w.views.contains(table), |d| {
                        d.child(
                            div()
                                .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                                .text_color(theme.muted_foreground)
                                .child("view"),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.transfer_focus_table(&name, window, cx)
                    })),
            );
        }
        let left =
            div()
                .flex()
                .flex_col()
                .w(px(300.))
                .flex_none()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2))
                        .child(Checkbox::new("tr-all").small().checked(all_on).on_click(
                            cx.listener(move |this, checked: &bool, _, cx| {
                                this.transfer_toggle_all(toggle_tables.clone(), *checked, cx)
                            }),
                        ))
                        .child(section(
                            &format!("Tables ({}/{})", w.selected.len(), w.tables.len()),
                            theme,
                        )),
                )
                .child(Input::new(&w.search).w_full())
                .child(list);
        let right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .child(self.render_transfer_table_config(theme, cx));
        div()
            .flex()
            .flex_row()
            .gap(px(SPACE_4))
            .size_full()
            .child(left)
            .child(right)
    }

    fn render_transfer_table_config(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let Some(table) = w.focus.clone() else {
            return div()
                .child(empty_state_text(
                    "Tick the tables to transfer. Click a table to set its transfer mode.".into(),
                    theme,
                ))
                .into_any_element();
        };
        let config = w.configs.get(&table);
        let advanced = config.is_some_and(|c| c.advanced);
        let mode_button = |id: &'static str, label: &'static str, adv: bool| {
            let table = table.clone();
            Button::new(id)
                .small()
                .label(label)
                .selected(advanced == adv)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.transfer_set_advanced(&table, adv, window, cx)
                }))
        };
        let mut col = div()
            .id("tr-config")
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .overflow_y_scroll()
            .child(section(&table, theme))
            .when(w.views.contains(&table), |d| {
                d.child(muted(
                    "A view: its rows are copied into a new table (no keys, indexes or triggers). Rows are read without a stable order.".into(),
                    theme,
                ))
            })
            .child(form_label("Transfer Mode", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(mode_button("tr-auto", "Auto", false))
                    .child(mode_button("tr-adv", "Advanced", true)),
            );
        let (config, true) = (config, advanced) else {
            return col
                .child(muted(
                    "Auto: same table name, all fields, all rows.".into(),
                    theme,
                ))
                .into_any_element();
        };
        let Some(c) = config else {
            return col.into_any_element();
        };
        col = col
            .child(form_label("Target Name", theme))
            .child(Input::new(&c.target).w_full())
            .child(form_label("Fields", theme));
        let toggle = |id: &'static str,
                      label: &'static str,
                      selected: bool,
                      f: fn(&mut Workbench, &str, bool, &mut Window, &mut Context<Workbench>),
                      value: bool| {
            let table = table.clone();
            Button::new(id)
                .small()
                .label(label)
                .selected(selected)
                .on_click(
                    cx.listener(move |this, _, window, cx| f(this, &table, value, window, cx)),
                )
        };
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(toggle(
                    "tr-f-all",
                    "All fields",
                    !c.custom_fields,
                    |this, t, v, _, cx| this.transfer_set_custom_fields(t, v, cx),
                    false,
                ))
                .child(toggle(
                    "tr-f-custom",
                    "Custom fields",
                    c.custom_fields,
                    |this, t, v, _, cx| this.transfer_set_custom_fields(t, v, cx),
                    true,
                )),
        );
        if c.custom_fields {
            let summary = match &c.fields {
                Some(f) => format!(
                    "{} of {} fields selected",
                    f.iter().filter(|f| f.on).count(),
                    f.len()
                ),
                None if c.fields_loading => "Loading fields…".to_string(),
                None => c.fields_error.clone().unwrap_or_default(),
            };
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new("tr-pick-fields")
                            .small()
                            .label("Choose fields…")
                            .disabled(c.fields.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.transfer_set_panel(Panel::Fields, cx)
                            })),
                    )
                    .child(muted(summary, theme)),
            );
        }
        col = col.child(form_label("Rows", theme)).child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(toggle(
                    "tr-r-all",
                    "All rows",
                    !c.custom_rows,
                    |this, t, v, window, cx| this.transfer_set_custom_rows(t, v, window, cx),
                    false,
                ))
                .child(toggle(
                    "tr-r-custom",
                    "Custom recordsets",
                    c.custom_rows,
                    |this, t, v, window, cx| this.transfer_set_custom_rows(t, v, window, cx),
                    true,
                )),
        );
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(muted("Rows per batch (optional)".into(), theme))
                .child(Input::new(&c.batch).w(px(90.))),
        );
        if c.custom_rows {
            col = col.child(muted(
                "Each recordset is a WHERE condition on the source table. They run one after another.".into(),
                theme,
            ));
            for (ix, entry) in c.recordsets.iter().enumerate() {
                let table = table.clone();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(SPACE_2))
                        .child(Input::new(entry).flex_1())
                        .child(
                            Button::new(("tr-rs-del", ix))
                                .small()
                                .ghost()
                                .icon(icon(IconName::X, 14., theme.muted_foreground))
                                .tooltip("Remove recordset")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.transfer_remove_recordset(&table, ix, cx)
                                })),
                        ),
                );
            }
            let add_table = table.clone();
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new("tr-rs-add")
                            .small()
                            .label("Add recordset")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.transfer_add_recordset(&add_table, window, cx)
                            })),
                    )
                    .child(
                        Button::new("tr-rs-gen")
                            .small()
                            .label("Recordset Generator…")
                            .disabled(c.fields.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transfer_open_generator(window, cx)
                            })),
                    ),
            );
        }
        col.into_any_element()
    }

    fn panel_shell(
        title: &str,
        body: impl IntoElement,
        buttons: impl IntoElement,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .size_full()
            .gap(px(SPACE_3))
            .child(section(title, theme))
            .child(div().flex().flex_col().flex_1().min_h(px(0.)).child(body))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(SPACE_2))
                    .child(buttons),
            )
    }

    fn render_transfer_options(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let o = &w.options;
        type Set = fn(&mut ora_core::transfer::TransferOptions, bool);
        let check =
            |id: &'static str, label: &str, value: bool, set: Set, indent: bool, enabled: bool| {
                div().pl(px(if indent { 22. } else { 0. })).child(
                    Checkbox::new(id)
                        .small()
                        .label(label.to_string())
                        .checked(value)
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(w) = this.transfer.as_mut() {
                                set(&mut w.options, *checked);
                            }
                            cx.notify();
                        })),
                )
            };
        let table_on = o.create_tables;
        let record_on = o.create_records;
        let case_button = |id: &'static str, label: &'static str, case: NameCase| {
            Button::new(id)
                .small()
                .label(label)
                .selected(o.convert_case == Some(case))
                .disabled(o.convert_case.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(w) = this.transfer.as_mut() {
                        w.options.convert_case = Some(case);
                    }
                    cx.notify();
                }))
        };
        let left = div()
            .flex()
            .flex_col()
            .flex_1()
            .gap(px(SPACE_2))
            .min_w(px(0.))
            .child(section("Table Options", theme))
            .child(check(
                "op-create",
                "Create tables",
                o.create_tables,
                |o, v| o.create_tables = v,
                false,
                true,
            ))
            .child(check(
                "op-index",
                "Include indexes",
                o.include_indexes,
                |o, v| o.include_indexes = v,
                true,
                table_on,
            ))
            .child(check(
                "op-pk",
                "Include primary key",
                o.include_primary_keys,
                |o, v| o.include_primary_keys = v,
                true,
                table_on,
            ))
            .child(check(
                "op-default",
                "Include default values",
                o.include_defaults,
                |o, v| o.include_defaults = v,
                true,
                table_on,
            ))
            .child(check(
                "op-fk",
                "Include foreign key constraints",
                o.include_foreign_keys,
                |o, v| o.include_foreign_keys = v,
                true,
                table_on,
            ))
            .child(check(
                "op-unique",
                "Include uniques",
                o.include_uniques,
                |o, v| o.include_uniques = v,
                true,
                table_on,
            ))
            .child(check(
                "op-check",
                "Include checks",
                o.include_checks,
                |o, v| o.include_checks = v,
                true,
                table_on,
            ))
            .child(check(
                "op-trigger",
                "Include triggers",
                o.include_triggers,
                |o, v| o.include_triggers = v,
                true,
                table_on,
            ))
            .child(check(
                "op-case",
                "Convert object names to",
                o.convert_case.is_some(),
                |o, v| o.convert_case = v.then_some(NameCase::Lower),
                false,
                true,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .pl(px(22.))
                    .child(case_button("op-lower", "Lower case", NameCase::Lower))
                    .child(case_button("op-upper", "Upper case", NameCase::Upper)),
            );
        let right = div()
            .flex()
            .flex_col()
            .flex_1()
            .gap(px(SPACE_2))
            .min_w(px(0.))
            .child(section("Record Options", theme))
            .child(check("op-records", "Create records", o.create_records, |o, v| o.create_records = v, false, true))
            .child(check("op-tx", "Use transaction (one per table)", o.use_transaction, |o, v| o.use_transaction = v, true, record_on))
            .child(check("op-complete", "Use complete insert statements", o.complete_inserts, |o, v| o.complete_inserts = v, true, record_on))
            .child(section("Other Options", theme))
            .child(check("op-continue", "Continue on error", o.continue_on_error, |o, v| o.continue_on_error = v, false, true))
            .child(check("op-lock", "Lock source tables", o.lock_source, |o, v| o.lock_source = v, false, true))
            .child(check("op-drop", "Drop target objects before create", o.drop_target, |o, v| o.drop_target = v, false, table_on))
            .child(check("op-empty", "Empty target tables before creating records", o.empty_target, |o, v| o.empty_target = v, false, record_on))
            .child(div().text_size(crate::theme::ui_size(theme, FONT_CAPTION)).text_color(theme.muted_foreground).child("Dropping or emptying target tables asks for confirmation before the run starts."));
        Self::panel_shell(
            "Option",
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_3))
                .child(div().flex().flex_row().gap(px(SPACE_4)).child(left).child(right))
                .child(muted(
                    "Include checks and default values apply between the same kind of database (Oracle to Oracle, SQLite to SQLite). Foreign keys are created only to tables in this transfer. Triggers are copied after the rows, between the same kind of database, and only for tables that keep their name and all columns.".into(),
                    theme,
                )),
            Button::new("op-ok").small().primary().label("OK").on_click(
                cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx)),
            ),
            theme,
        )
    }

    fn render_transfer_fields(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let table = w.focus.clone().unwrap_or_default();
        let mut list = div()
            .id("tr-fields")
            .flex()
            .flex_col()
            .gap(px(4.))
            .overflow_y_scroll()
            .size_full();
        if let Some(fields) = w.configs.get(&table).and_then(|c| c.fields.as_ref()) {
            for (ix, f) in fields.iter().enumerate() {
                let table = table.clone();
                list = list.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2))
                        .flex_none()
                        .child(
                            Checkbox::new(("tr-fld", ix))
                                .small()
                                .checked(f.on)
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.transfer_toggle_field(&table, ix, *checked, cx)
                                })),
                        )
                        .child(
                            div()
                                .w(px(260.))
                                .truncate()
                                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                .text_color(theme.foreground)
                                .child(format!("{}  ·  {}", f.source, f.data_type)),
                        )
                        .child(icon(IconName::ArrowRight, 14., theme.muted_foreground))
                        .child(Input::new(&f.target).flex_1()),
                );
            }
        }
        let (a, b) = (table.clone(), table.clone());
        Self::panel_shell(
            &format!("Fields of {table}: source → target"),
            list,
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(Button::new("fd-all").small().label("Select All").on_click(
                    cx.listener(move |this, _, _, cx| this.transfer_all_fields(&a, true, cx)),
                ))
                .child(
                    Button::new("fd-none")
                        .small()
                        .label("Deselect All")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.transfer_all_fields(&b, false, cx)
                        })),
                )
                .child(Button::new("fd-ok").small().primary().label("OK").on_click(
                    cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx)),
                )),
            theme,
        )
    }

    fn render_transfer_generator(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let table = w.focus.clone().unwrap_or_default();
        let Some(g) = w.generator.as_ref() else {
            return div().into_any_element();
        };
        let fields: Vec<(String, bool, Pick)> = w
            .configs
            .get(&table)
            .and_then(|c| c.fields.as_ref())
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| {
                        let name = f.source.clone();
                        let action: Pick = Rc::new(move |this, cx| {
                            if let Some(g) =
                                this.transfer.as_mut().and_then(|w| w.generator.as_mut())
                            {
                                g.field = Some(name.clone());
                            }
                            cx.notify();
                        });
                        (
                            format!("{}  ·  {}", f.source, f.data_type),
                            g.field.as_ref() == Some(&f.source),
                            action,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let field_dropdown = self.transfer_dropdown(
            "gen-field",
            g.field.clone().unwrap_or_else(|| "Choose…".into()),
            fields,
            theme,
            cx,
        );
        let kind_button = |id: &'static str, label: &'static str, kind: RangeKind| {
            Button::new(id)
                .small()
                .label(label)
                .selected(g.kind == kind)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(g) = this.transfer.as_mut().and_then(|w| w.generator.as_mut()) {
                        g.kind = kind;
                    }
                    cx.notify();
                }))
        };
        let hint = match g.kind {
            RangeKind::Integer => "Whole numbers, e.g. 1 and 100000.",
            RangeKind::Date => "Dates as YYYY-MM-DD, e.g. 2024-01-01.",
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .w(px(460.))
            .child(form_label("Field name", theme))
            .child(field_dropdown)
            .child(form_label("Type", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(kind_button("gen-int", "Integer", RangeKind::Integer))
                    .child(kind_button("gen-date", "Date", RangeKind::Date)),
            )
            .child(muted(hint.into(), theme))
            .child(form_label("Start value", theme))
            .child(Input::new(&g.start).w_full())
            .child(form_label("End value", theme))
            .child(Input::new(&g.end).w_full())
            .child(form_label("Number of recordsets", theme))
            .child(Input::new(&g.count).w_full())
            .children(g.error.clone().map(|e| {
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .text_color(theme.danger)
                    .child(e)
            }));
        Self::panel_shell(
            "Recordset Generator",
            body,
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(Button::new("gen-cancel").small().label("Cancel").on_click(
                    cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx)),
                ))
                .child(
                    Button::new("gen-ok")
                        .small()
                        .primary()
                        .label("Generate")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.transfer_generate(&table, window, cx)
                        })),
                ),
            theme,
        )
        .into_any_element()
    }

    fn render_transfer_save(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        Self::panel_shell(
            "Save Profile",
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .w(px(420.))
                .child(form_label("Profile name", theme))
                .child(Input::new(&w.profile_name).w_full())
                .child(muted(
                    "Saves the connections' names, schemas, tables, field mapping, recordsets and options. Passwords are never saved."
                        .into(),
                    theme,
                )),
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(Button::new("sp-cancel").small().label("Cancel").on_click(
                    cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx)),
                ))
                .child(Button::new("sp-ok").small().primary().label("Save").on_click(
                    cx.listener(|this, _, _, cx| this.transfer_save_profile(cx)),
                )),
            theme,
        )
    }

    fn render_transfer_load(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let mut list = div()
            .id("tr-profiles")
            .flex()
            .flex_col()
            .gap(px(4.))
            .overflow_y_scroll()
            .size_full();
        if w.profiles.is_empty() {
            list = list.child(empty_state_text("No saved profiles yet.".into(), theme));
        }
        for (ix, p) in w.profiles.iter().enumerate() {
            let target = match &p.target {
                ora_core::transfer::TransferTarget::Connection(n) => n.clone(),
                ora_core::transfer::TransferTarget::SqlFile(path) => format!("file {path}"),
                ora_core::transfer::TransferTarget::CsvDir(path) => format!("CSV folder {path}"),
            };
            list = list.child(
                Button::new(("tr-prof", ix))
                    .small()
                    .w_full()
                    .label(format!(
                        "{}   ({} → {}, {} tables)",
                        p.name,
                        p.source_connection,
                        target,
                        p.spec.tables.len()
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.transfer_load_profile(ix, window, cx)
                    })),
            );
        }
        Self::panel_shell(
            "Load Profile",
            list,
            Button::new("lp-cancel")
                .small()
                .label("Cancel")
                .on_click(cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx))),
            theme,
        )
    }

    fn render_transfer_confirm(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let spec = self.build_transfer_spec(cx).ok();
        let o = &w.options;
        let source = w
            .source
            .and_then(|id| self.open_conn(id))
            .map_or("—".to_string(), |c| c.label);
        let target = w
            .target
            .and_then(|id| self.open_conn(id))
            .map_or("—".to_string(), |c| c.label);
        let schema = spec
            .as_ref()
            .map(|s| s.target_schema.clone())
            .unwrap_or_default();
        let prod = self.target_is_production();
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .w(px(560.))
            .child(body_text(
                format!(
                    "Copy {} table(s) from {} ({}) to {} ({}).",
                    spec.as_ref().map_or(0, |s| s.tables.len()),
                    source,
                    w.source_schema.clone().unwrap_or_default(),
                    target,
                    schema
                ),
                theme,
            ));
        if o.create_tables && o.drop_target {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .child(format!("Existing tables with the same names in {schema} are dropped and recreated.")),
            );
        }
        if o.create_records && o.empty_target {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .child(format!(
                        "Existing rows of those tables in {schema} are deleted first."
                    )),
            );
        }
        if prod {
            body = body.child(
                div()
                    .px(px(SPACE_3))
                    .py(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .bg(theme.danger.opacity(0.12))
                    .text_color(theme.danger)
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                    .child("The target is a PRODUCTION connection."),
            );
            if o.destroys_target_data() {
                body = body
                    .child(form_label(&format!("Type {schema} to confirm"), theme))
                    .child(Input::new(&w.confirm_input).w_full());
            }
        }
        Self::panel_shell(
            "Confirm Data Transfer",
            body,
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(Button::new("cf-cancel").small().label("Cancel").on_click(
                    cx.listener(|this, _, _, cx| this.transfer_set_panel(Panel::None, cx)),
                ))
                .child(
                    Button::new("cf-go")
                        .small()
                        .danger()
                        .label("Start transfer")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.transfer_start(window, cx)),
                        ),
                ),
            theme,
        )
    }

    fn render_transfer_run(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let w = self.transfer.as_ref().expect("wizard open");
        let spec = self.build_transfer_spec(cx).ok();
        let o = &w.options;
        let yes = |v: bool| if v { "yes" } else { "no" };
        let mut col = div().flex().flex_col().gap(px(SPACE_2)).size_full();
        if let Some(spec) = &spec {
            col = col.child(section("Summary", theme)).child(body_text(
                format!(
                    "{} table(s) from {} to {}.",
                    spec.tables.len(),
                    spec.source_schema,
                    match w.target_mode {
                        TargetMode::Connection => spec.target_schema.clone(),
                        TargetMode::File if w.file_csv => "CSV files".to_string(),
                        TargetMode::File => "an SQL file".to_string(),
                    }
                ),
                theme,
            ));
            col = col.child(muted(
                format!(
                    "Create tables: {}   Indexes: {}   Keys and constraints: {}   Create records: {}   Transaction: {}   Continue on error: {}   Drop target: {}   Empty target: {}",
                    yes(o.create_tables),
                    yes(o.include_indexes),
                    yes(o.include_primary_keys
                        || o.include_uniques
                        || o.include_checks
                        || o.include_foreign_keys),
                    yes(o.create_records),
                    yes(o.use_transaction),
                    yes(o.continue_on_error),
                    yes(o.drop_target),
                    yes(o.empty_target),
                ),
                theme,
            ));
        }
        if let Some(run) = &w.run {
            let status = match (&run.summary, &run.table) {
                (Some(s), _) if s.invalid.is_some() => "Not started".to_string(),
                (Some(s), _) if s.cancelled => format!("Stopped: {} rows copied", s.rows()),
                (Some(s), _) if s.failed_tables() > 0 => format!(
                    "Finished with errors: {} rows copied, {} table(s) failed",
                    s.rows(),
                    s.failed_tables()
                ),
                (Some(s), _) => format!("Done: {} rows copied", s.rows()),
                (None, Some((ix, total, name))) => {
                    format!("Table {ix} of {total}: {name}, {} rows so far", run.rows)
                }
                (None, None) => "Starting…".to_string(),
            };
            let elapsed = run.elapsed.unwrap_or_else(|| run.started.elapsed());
            let stat = |label: &str, value: String| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .child(format!("{label}:")),
                    )
                    .child(body_text(value, theme))
            };
            let errors = run.skipped + run.failed as u64;
            let fraction = if run.objects == 0 {
                0.
            } else {
                run.done_tables as f32 / run.objects as f32
            };
            col = col
                .child(section("Progress", theme))
                .child(stat("Objects", run.objects.to_string()))
                .child(stat(
                    "Processed Records",
                    (run.total_rows + run.rows + run.skipped).to_string(),
                ))
                .child(stat(
                    "Transferred Records",
                    (run.total_rows + run.rows).to_string(),
                ))
                .child(stat("Errors", errors.to_string()))
                .child(stat("Time", format!("{:.2}s", elapsed.as_secs_f32())))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SPACE_2))
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded_full()
                                .bg(theme.muted)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(if errors > 0 {
                                            theme.warning
                                        } else {
                                            theme.primary
                                        })
                                        .w(relative(fraction.clamp(0., 1.))),
                                ),
                        )
                        .when(run.running, |d| {
                            d.child(
                                gpui_kit::component::spinner::Spinner::new().color(theme.primary),
                            )
                        }),
                )
                .child(body_text(status, theme));
            let mut log = div()
                .id("tr-log")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .border_1()
                .border_color(theme.border)
                .rounded(px(RADIUS_SM))
                .p(px(SPACE_2));
            for (level, text) in &run.log {
                let color = match level {
                    LogLevel::Info => theme.foreground,
                    LogLevel::Warn => theme.warning,
                    LogLevel::Error => theme.danger,
                };
                log = log.child(
                    div()
                        .flex_none()
                        .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                        .text_color(color)
                        .child(text.clone()),
                );
            }
            col = col.child(log);
        } else {
            col = col.child(empty_state_text(
                "Press Start to run the transfer. Stop ends it between batches.".into(),
                theme,
            ));
        }
        col
    }
}
