use super::*;

pub(super) const EXPLORER_CONTROL_HEIGHT: f32 = 30.;

pub(super) fn control(id: &'static str, theme: &gpui_kit::component::Theme) -> Button {
    Button::new(id)
        .ghost()
        .bg(theme.background)
        .rounded(px(6.))
        .h(px(EXPLORER_CONTROL_HEIGHT))
}

impl Workbench {
    pub(super) fn schema_picker(
        &self,
        id: &'static str,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let schemas = self.conn.schemas.clone();
        let current = self.conn.current_schema.clone();
        let label = current.clone().unwrap_or_else(|| "(schema)".to_string());
        let button = if id == "schema-select" {
            control(id, theme).w_full().child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(6.))
                    .child(icon(IconName::Layers, 14., theme.muted_foreground))
                    .child(select_trigger(label, theme).flex_1()),
            )
        } else {
            Button::new(id)
                .ghost()
                .small()
                .child(select_trigger(label, theme))
        };
        button
            .disabled(self.conn.schemas.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(220.));
                for schema in &schemas {
                    let pick = schema.clone();
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(schema.clone())
                            .checked(current.as_deref() == Some(schema.as_str()))
                            .on_click(move |_, _, cx: &mut App| {
                                let pick = pick.clone();
                                view.update(cx, |this, cx| this.select_schema(pick, cx));
                            }),
                    );
                }
                menu
            })
    }

    /// Schema and category selectors, Quick Open, and the object list.
    pub(super) fn render_explorer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let counts: Vec<(ObjectCategory, usize)> = ObjectCategory::ALL
            .iter()
            .map(|c| (*c, schema::count_in(&self.conn.objects, *c)))
            .collect();
        let current_category = self.category;
        let category_button = control("category-select", theme)
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(current_category.label()),
                    )
                    .child(caption_text(
                        schema::count_in(&self.conn.objects, current_category).to_string(),
                        theme,
                    ))
                    .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
            )
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(220.));
                for (category, count) in &counts {
                    let category = *category;
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} ({count})", category.label()))
                            .checked(category == current_category)
                            .on_click(move |_, _, cx: &mut App| {
                                view.update(cx, |this, cx| this.select_category(category, cx));
                            }),
                    );
                }
                menu
            });

        let visible: Vec<ObjectRef> = schema::objects_in(&self.conn.objects, self.category, "")
            .into_iter()
            .map(|o| ObjectRef {
                owner: o.owner.clone(),
                name: o.name.clone(),
                kind: o.kind.clone(),
            })
            .collect();
        let list = if visible.is_empty() {
            let message = if self.conn.busy && self.conn.objects.is_empty() {
                "Memuat objek..."
            } else {
                "Tidak ada objek di kategori ini."
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(SPACE_4))
                .child(empty_state_text(message.to_string(), theme))
                .into_any_element()
        } else {
            let count = visible.len();
            let selected = self.conn.selected_object.clone();
            let theme = theme.clone();
            div()
                .flex_1()
                .px(px(SPACE_2))
                .child(
                    uniform_list(
                        "explorer-objects",
                        count,
                        cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .map(|ix| {
                                    let object = visible[ix].clone();
                                    let is_selected =
                                        selected.as_deref() == Some(object.name.as_str());
                                    render_object_row(ix, object, is_selected, &theme, cx)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.object_scroll)
                    .size_full(),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(8.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(SPACE_2))
                    .flex_none()
                    .child(self.schema_picker("schema-select", theme, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(SPACE_2))
                    .flex_none()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(category_button),
                    )
                    .child(
                        control("explorer-quick-open", theme)
                            .icon(icon(IconName::Search, 15., theme.muted_foreground))
                            .size(px(EXPLORER_CONTROL_HEIGHT))
                            .flex_none()
                            .tooltip("Quick Open (Ctrl/Cmd+P)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_quick_open(window, cx)
                            })),
                    ),
            )
            .child(list)
    }
}

/// One explorer row: icon + name. Double click opens data (tables/views)
/// or the script (everything else); right click opens the Toad-style
/// object menu.
fn render_object_row(
    ix: usize,
    object: ObjectRef,
    selected: bool,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    let icon_name = match object.kind {
        ObjectKind::Table => IconName::Table,
        ObjectKind::View => IconName::Eye,
        ObjectKind::MaterializedView => IconName::Columns3,
        ObjectKind::Index => IconName::ListOrdered,
        ObjectKind::Sequence => IconName::Hash,
        ObjectKind::Procedure | ObjectKind::Function => IconName::SquareFunction,
        ObjectKind::Package => IconName::Braces,
        ObjectKind::Trigger => IconName::Zap,
        ObjectKind::Synonym => IconName::Link,
        ObjectKind::Other(_) => IconName::FileText,
    };
    let click_object = object.clone();
    let mut row = div()
        .id(("explorer-row", ix))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE_2))
        .h(px(TREE_ROW_HEIGHT))
        .px(px(SPACE_2))
        .rounded(px(RADIUS_SM))
        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
        .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
        .on_click(cx.listener(move |this, event, window, cx| {
            this.on_object_click(click_object.clone(), event, window, cx);
        }));
    if selected {
        row = row.bg(theme.accent).text_color(theme.accent_foreground);
    } else {
        row = row
            .text_color(theme.foreground)
            .hover(|this| this.bg(theme.secondary));
    }
    let view = cx.entity();
    row.child(icon(icon_name, 14., theme.muted_foreground))
        .child(div().flex_1().truncate().child(object.name.clone()))
        .context_menu(move |menu, _, _| object_menu(menu, &view, &object))
}

/// The right-click menu for an explorer object.
fn object_menu(menu: PopupMenu, view: &Entity<Workbench>, object: &ObjectRef) -> PopupMenu {
    let has_data = schema::has_data(&object.kind);
    let is_table = object.kind == ObjectKind::Table;
    let item =
        |label: &str,
         icon_name: IconName,
         enabled: bool,
         f: fn(&mut Workbench, ObjectRef, &mut Window, &mut Context<Workbench>)| {
            let view = view.clone();
            let object = object.clone();
            PopupMenuItem::new(label.to_string())
                .icon(Icon::new(icon_name))
                .disabled(!enabled)
                .on_click(move |_, window, cx: &mut App| {
                    let object = object.clone();
                    view.update(cx, |this, cx| f(this, object, window, cx));
                })
        };
    menu.min_w(px(220.))
        .item(item(
            "Open Data",
            IconName::Table,
            has_data,
            |this, o, window, cx| this.open_table_data(&o.owner, &o.name, window, cx),
        ))
        .item(item(
            "Describe",
            IconName::Columns3,
            has_data,
            |this, o, window, cx| {
                this.open_table_data_in(&o.owner, &o.name, DataView::Structure, window, cx)
            },
        ))
        .item(item(
            "View Script",
            IconName::FileCode,
            true,
            |this, o, _, cx| this.view_script(o, cx),
        ))
        .separator()
        .item(item(
            "Export Data (CSV)",
            IconName::Download,
            has_data,
            |this, o, _, cx| this.export_table(o, ExportFormat::Csv, cx),
        ))
        .item(item(
            "Export Data (XLSX)",
            IconName::FileSpreadsheet,
            has_data,
            |this, o, _, cx| this.export_table(o, ExportFormat::Xlsx, cx),
        ))
        .item(item(
            "Import Data (CSV)",
            IconName::Upload,
            is_table,
            |this, o, _, cx| this.import_table(o, cx),
        ))
}

impl Workbench {
    /// Reload the schema list, then the objects of the current schema
    /// (the session user's own schema on first load).
    pub(super) fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if self.conn.busy {
            return;
        }
        if self.conn.session.is_none() {
            self.push_message("Belum terhubung.".to_string());
            return;
        }
        self.conn.status = "Memuat daftar schema...".to_string();
        self.spawn_db(Job::ListSchemas, PendingOp::ListSchemas, cx);
    }

    pub(super) fn load_objects(&mut self, cx: &mut Context<Self>) {
        let Some(owner) = self.conn.current_schema.clone() else {
            return;
        };
        self.conn.status = format!("Memuat objek {owner}...");
        self.spawn_db(Job::ListObjects { owner }, PendingOp::ListObjects, cx);
    }

    fn select_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.current_schema.as_deref() == Some(schema.as_str()) {
            return;
        }
        self.conn.current_schema = Some(schema);
        self.conn.selected_object = None;
        self.conn.objects.clear();
        self.load_objects(cx);
    }

    fn select_category(&mut self, category: ObjectCategory, cx: &mut Context<Self>) {
        self.category = category;
        self.conn.selected_object = None;
        cx.notify();
    }

    /// Single click selects; double click on a table/view opens its data
    /// in a new tab, like Toad. The platform reports the tap count on the
    /// mouse-down event, so a double click arrives with `click_count >= 2`.
    fn on_object_click(
        &mut self,
        object: ObjectRef,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conn.selected_object = Some(object.name.clone());
        let double_click = matches!(event, ClickEvent::Mouse(m) if m.down.click_count >= 2);
        if double_click {
            if schema::has_data(&object.kind) {
                self.open_table_data(&object.owner, &object.name, window, cx);
            } else {
                self.view_script(object, cx);
            }
        }
        cx.notify();
    }

    /// Open `owner.name` in a new data tab titled after the table and run a
    /// capped `SELECT *` there.
    pub(super) fn open_table_data(
        &mut self,
        owner: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_table_data_in(owner, name, DataView::Data, window, cx);
    }

    /// Like [`Self::open_table_data`], starting on `view` (Describe opens
    /// the Structure view; the rows load when Data is first shown).
    fn open_table_data_in(
        &mut self,
        owner: &str,
        name: &str,
        start_view: DataView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conn.session.is_none() {
            return;
        }
        if self.conn.busy {
            self.conn.status = "Masih ada operasi berjalan.".to_string();
            cx.notify();
            return;
        }
        let kind = self
            .conn
            .objects
            .iter()
            .find(|o| o.owner == owner && o.name == name)
            .map_or(ObjectKind::Table, |o| o.kind.clone());
        self.add_tab(window, cx);
        let view = cx.entity().downgrade();
        let editable = kind == ObjectKind::Table;
        let data = DataTab::new(owner, name, kind, window, cx);
        let table_state = self.active_tab().table_state.clone();
        let observe = cx.observe(&table_state, |_, _, cx| cx.notify());
        let mut data = data;
        data._grid_observe = Some(observe);
        let tab = self.active_tab_mut();
        tab.title = format!("{owner}.{name}");
        tab.kind = TabKind::Data;
        tab.data = Some(data);
        let edit_view = view.clone();
        let table_name = format!(
            "{}.{}",
            ora_core::import::quote_ident(owner),
            ora_core::import::quote_ident(name)
        );
        tab.table_state.update(cx, |state, _| {
            let mut delegate = ResultsDelegate::empty()
                .with_page_size(DATA_PAGE_SIZE)
                .with_row_numbers()
                .with_quick_filter(Rc::new(move |condition, window, cx| {
                    let _ = view.update(cx, |this, cx| this.quick_filter(condition, window, cx));
                }));
            // Only tables have a stable ROWID to find a row by.
            if editable {
                delegate = delegate.with_cell_edit(Rc::new(move |request, window, cx| {
                    let _ =
                        edit_view.update(cx, |this, cx| this.open_cell_editor(request, window, cx));
                }));
            }
            delegate.set_table_name(Some(table_name));
            *state.delegate_mut() = delegate;
        });
        self.add_filter_row(None, None, window, cx);
        if start_view == DataView::Data {
            self.reload_data(cx);
        } else {
            if let Some(data) = self.data_mut() {
                data.view = start_view;
            }
            self.load_data_view(cx);
        }
    }

    /// "View Script": fetch the DDL; the response opens it in a new tab.
    fn view_script(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        self.conn.status = format!("Mengambil DDL {}.{}...", object.owner, object.name);
        self.spawn_db(
            Job::Ddl {
                owner: object.owner.clone(),
                name: object.name.clone(),
                kind: object.kind,
            },
            PendingOp::Ddl {
                owner: object.owner,
                name: object.name,
            },
            cx,
        );
    }

    /// "Export Data": ask where to save (native save dialog), then let the
    /// worker fetch and write the whole table. If the platform has no file
    /// dialog, fall back to a timestamped file in the working directory.
    pub(super) fn export_table(
        &mut self,
        object: ObjectRef,
        format: ExportFormat,
        cx: &mut Context<Self>,
    ) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let conn_id = self.conn.id;
        let suggested = format!("{}.{}", object.name, format.extension());
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |view, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => Some(path),
                Ok(Ok(None)) => None, // cancelled
                _ => Some(dir.join(format!(
                    "{}-{}.{}",
                    object.name,
                    unix_stamp(),
                    format.extension()
                ))),
            };
            let Some(path) = path else {
                return;
            };
            let _ = view.update(cx, |this, cx| {
                this.with_conn(conn_id, cx, |this, _, cx| {
                    this.conn.status = format!("Export {}.{}...", object.owner, object.name);
                    this.spawn_db(
                        Job::ExportTable {
                            owner: object.owner.clone(),
                            name: object.name.clone(),
                            path,
                            format,
                        },
                        PendingOp::ExportTable {
                            owner: object.owner,
                            name: object.name,
                        },
                        cx,
                    );
                });
            });
        })
        .detach();
    }

    /// "Import Data": pick a CSV file whose header names match the table's
    /// columns; rows are inserted without commit.
    pub(super) fn import_table(&mut self, object: ObjectRef, cx: &mut Context<Self>) {
        if self.conn.busy || self.conn.session.is_none() {
            return;
        }
        let conn_id = self.conn.id;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import CSV".into()),
        });
        cx.spawn(async move |view, cx| {
            let picked = prompt.await;
            let _ = view.update(cx, |this, cx| {
                let path = match picked {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) => None,
                    _ => {
                        this.push_message(
                            "Dialog pilih file tidak tersedia di sistem ini.".to_string(),
                        );
                        cx.notify();
                        None
                    }
                };
                let Some(path) = path else {
                    return;
                };
                this.with_conn(conn_id, cx, |this, _, cx| {
                    this.conn.status = format!("Import ke {}.{}...", object.owner, object.name);
                    this.spawn_db(
                        Job::ImportCsv {
                            owner: object.owner.clone(),
                            name: object.name.clone(),
                            path,
                        },
                        PendingOp::ImportCsv {
                            owner: object.owner,
                            name: object.name,
                        },
                        cx,
                    );
                });
            });
        })
        .detach();
    }
}
