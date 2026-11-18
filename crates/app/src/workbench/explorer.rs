use super::*;

impl Workbench {
    /// Toad-style object browser: schema dropdown, category tabs, filter
    /// box, and a flat virtualized list of objects with a context menu.
    pub(super) fn render_explorer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let schemas = self.conn.schemas.clone();
        let current = self.conn.current_schema.clone();
        let schema_button = Button::new("schema-select")
            .child(select_trigger(
                current.clone().unwrap_or_else(|| "(schema)".to_string()),
                theme,
            ))
            .w_full()
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
            });

        let view = cx.entity();
        let counts: Vec<(ObjectCategory, usize)> = ObjectCategory::ALL
            .iter()
            .map(|c| (*c, schema::count_in(&self.conn.objects, *c)))
            .collect();
        let current_category = self.category;
        let category_button = Button::new("category-select")
            .child(select_trigger(
                format!(
                    "{} ({})",
                    current_category.label(),
                    schema::count_in(&self.conn.objects, current_category)
                ),
                theme,
            ))
            .w_full()
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

        let visible: Vec<ObjectRef> =
            schema::objects_in(&self.conn.objects, self.category, &self.tree_filter)
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
            } else if self.tree_filter.is_empty() {
                "Tidak ada objek di kategori ini."
            } else {
                "Tidak ada objek yang cocok dengan filter."
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
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .pt(px(SPACE_3))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_3))
                    .child(
                        div()
                            .w(px(60.))
                            .whitespace_nowrap()
                            .flex_none()
                            .child(form_label("Schema", theme)),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(schema_button)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_3))
                    .child(
                        div()
                            .w(px(60.))
                            .whitespace_nowrap()
                            .flex_none()
                            .child(form_label("Objects", theme)),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(category_button)),
            )
            .child(
                div()
                    .px(px(SPACE_3))
                    .child(Input::new(&self.search_input).w_full()),
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
        .text_size(px(FONT_LABEL))
        .line_height(px(LEADING_LABEL))
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
