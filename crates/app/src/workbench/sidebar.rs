use super::*;

use gpui_kit::component::{button::ButtonCustomVariant, FocusableExt as _};

pub(super) fn tree_button(button: Button, keyboard: bool, cx: &App) -> Button {
    button
        .custom(ButtonCustomVariant::new(cx))
        .focus_ring(keyboard)
}

impl Workbench {
    pub(super) fn render_navigation(
        &self,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let compact = self.sidebar_compact;
        let favorites = self.history.iter().filter(|entry| entry.favorite).count();
        let history = self.history.len();
        let mut nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .min_h(px(0.))
            .w(px(if compact {
                RAIL_WIDTH
            } else {
                CONNECTIONS_WIDTH
            }))
            .bg(theme.sidebar.blend(theme.foreground.opacity(0.06)))
            .py(px(SPACE_2))
            .px(px(if compact { 5. } else { 6. }))
            .gap(px(SPACE_1));

        for (panel, symbol, label, count) in [
            (
                RailPanel::Connections,
                IconName::Database,
                "Connections",
                None,
            ),
            (
                RailPanel::Favorites,
                IconName::Star,
                "Favorites",
                Some(favorites),
            ),
            (
                RailPanel::History,
                IconName::Clock,
                "History",
                Some(history),
            ),
        ] {
            let selected = self.rail == panel;
            let button = tree_button(Button::new(label), keyboard, cx)
                .small()
                .tooltip(label)
                .w_full()
                .h(px(if compact { 34. } else { 30. }))
                .when(selected, |button| button.text_color(theme.primary))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rail = panel;
                    this.sidebar_compact = false;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(6.))
                        .when(compact, |row| row.justify_center())
                        .child(icon(
                            symbol,
                            16.,
                            if selected {
                                theme.primary
                            } else {
                                theme.muted_foreground
                            },
                        ))
                        .when(!compact, |row| {
                            row.child(div().flex_1().text_left().child(label))
                                .children(count.map(|count| caption_text(count.to_string(), theme)))
                        }),
                );
            let mut heading = div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(2.))
                .child(div().flex_1().child(button));
            if panel == RailPanel::Connections && !compact {
                heading = heading.child(
                    tree_button(Button::new("profile-new"), keyboard, cx)
                        .small()
                        .icon(IconName::Plus)
                        .tooltip("New connection")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_new_connection(window, cx)),
                        ),
                );
                let import_view = cx.entity();
                let export_view = import_view.clone();
                let has_profiles = !self.profiles.is_empty();
                heading = heading.child(
                    tree_button(Button::new("connection-transfer"), keyboard, cx)
                        .small()
                        .icon(IconName::Ellipsis)
                        .tooltip("Import or export connections")
                        .dropdown_menu(move |menu, _, _| {
                            let import_view = import_view.clone();
                            let export_view = export_view.clone();
                            menu.item(
                                PopupMenuItem::new("Import Ora connections…")
                                    .icon(Icon::new(IconName::Upload))
                                    .on_click(move |_, _, cx| {
                                        import_view
                                            .update(cx, |this, cx| this.import_connections(cx))
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Export Ora connections…")
                                    .disabled(!has_profiles)
                                    .icon(Icon::new(IconName::Download))
                                    .on_click(move |_, _, cx| {
                                        export_view
                                            .update(cx, |this, cx| this.export_connections(cx))
                                    }),
                            )
                        }),
                );
            }
            if panel == RailPanel::Connections {
                let view = cx.entity();
                nav = nav.child(
                    div()
                        .id("connections-heading-menu")
                        .child(heading)
                        .context_menu(move |menu, _, _| {
                            menu.item(connection_tree::new_folder_item(&view, ""))
                        }),
                );
            } else {
                nav = nav.child(heading);
            }
            if selected && !compact && panel == RailPanel::Connections {
                nav = nav.child(self.render_profile_list(keyboard, theme, cx));
            }
        }
        nav.child(div().flex_1().min_h(px(0.))).child(
            tree_button(Button::new("sidebar-settings"), keyboard, cx)
                .small()
                .w_full()
                .h(px(34.))
                .tooltip("Settings")
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(6.))
                        .when(compact, |row| row.justify_center())
                        .child(icon(IconName::Settings, 16., theme.muted_foreground))
                        .when(!compact, |row| row.child("Settings")),
                ),
        )
    }

    pub(super) fn render_history_panel(
        &self,
        favorites_only: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = if favorites_only {
            "Favorites"
        } else {
            "History"
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(SPACE_3))
            .child(form_label(title, theme))
            .when(!favorites_only, |header| {
                header.child(
                    Button::new("history-clear")
                        .small()
                        .ghost()
                        .icon(IconName::Trash)
                        .tooltip("Clear history")
                        .disabled(self.history.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx))),
                )
            });
        let mut panel = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(header);
        let entries: Vec<(usize, &HistoryEntry)> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, e)| !favorites_only || e.favorite)
            .collect();
        if entries.is_empty() {
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .p(px(SPACE_4))
                    .child(empty_state_text(
                        if favorites_only {
                            "No favorites yet. Star a query in History to pin it here."
                        } else {
                            "No history yet. Run a query to see it here."
                        }
                        .to_string(),
                        theme,
                    )),
            );
            return panel;
        }
        let mut list = div()
            .flex()
            .flex_col()
            .flex_1()
            .id(if favorites_only {
                "favorites-scroll"
            } else {
                "history-scroll"
            })
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(px(SPACE_2));
        for (visible_ix, (ix, entry)) in entries.into_iter().enumerate() {
            let preview = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let sql = entry.sql.clone();
            let favorite = entry.favorite;
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(px(RADIUS_SM))
                    .h(px(TREE_ROW_HEIGHT))
                    .flex_none()
                    .bg(if visible_ix % 2 == 0 {
                        theme.table_even
                    } else {
                        theme.table
                    })
                    .hover(|this| this.bg(theme.secondary))
                    .child(
                        div()
                            .id(format!("history-{ix}"))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .flex_1()
                            // Without min_w(0) a long script keeps its full
                            // width and pushes the star out of view.
                            .min_w(px(0.))
                            .overflow_hidden()
                            .py(px(SPACE_1))
                            .px(px(SPACE_2))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load_sql_into_active_tab(sql.clone(), window, cx);
                            }))
                            .child(
                                div()
                                    .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                                    .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
                                    .text_color(theme.foreground)
                                    .truncate()
                                    .child(preview),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("history-star-{ix}"))
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .h(px(28.))
                            .w(px(28.))
                            .mr(px(SPACE_1))
                            .rounded(px(RADIUS_MD))
                            .hover(|this| this.bg(theme.accent))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_favorite(ix, cx)),
                            )
                            .child(icon(
                                IconName::Star,
                                14.,
                                if favorite {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                },
                            )),
                    ),
            );
        }
        panel.child(list)
    }
}

impl Workbench {
    fn persist_history(&mut self) {
        if let Err(err) = store::save_history(&self.history) {
            self.push_message(format!("Gagal menyimpan history: {err}"));
        }
    }

    fn toggle_favorite(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.history.get_mut(ix) {
            entry.favorite = !entry.favorite;
            self.persist_history();
            cx.notify();
        }
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.history.clear();
        self.persist_history();
        cx.notify();
    }
}
