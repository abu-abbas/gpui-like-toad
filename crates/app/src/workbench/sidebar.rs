use super::*;

impl Workbench {
    pub(super) fn render_rail(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let items = [
            (RailPanel::Connections, IconName::Database, "Connections"),
            (RailPanel::History, IconName::Clock, "History"),
            (RailPanel::Favorites, IconName::Star, "Favorites"),
            (RailPanel::Settings, IconName::Settings, "Settings"),
        ];
        let mut rail = div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(RAIL_WIDTH))
            .py(px(SPACE_2))
            .gap(px(SPACE_1))
            // One step further from the content than the side panel, so
            // the rail, the panel and the work area are three shades.
            .bg(theme.sidebar.blend(theme.foreground.opacity(0.06)));
        for (panel, icon_name, label) in items {
            let active = self.rail == panel;
            let mut button = div()
                .id(format!("rail-{label}"))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .w(px(RAIL_WIDTH - 6.))
                .h(px(RAIL_BUTTON_HEIGHT))
                .gap(px(SPACE_1))
                .rounded(px(RADIUS_MD))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.rail = panel;
                    cx.notify();
                }));
            if active {
                button = button.bg(theme.accent);
            } else {
                button = button.hover(|this| this.bg(theme.secondary));
            }
            let color = if active {
                theme.primary
            } else {
                theme.muted_foreground
            };
            rail = rail.child(
                button.child(icon(icon_name, 20., color)).child(
                    div()
                        .text_size(px(FONT_RAIL))
                        .line_height(px(LEADING_RAIL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                ),
            );
        }
        rail
    }

    pub(super) fn render_side_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.rail {
            RailPanel::Connections => self.render_connections_panel(theme, cx).into_any_element(),
            RailPanel::History => self
                .render_history_panel(false, theme, cx)
                .into_any_element(),
            RailPanel::Favorites => self
                .render_history_panel(true, theme, cx)
                .into_any_element(),
            RailPanel::Settings => self.render_settings_panel(theme, cx).into_any_element(),
        }
    }

    pub(super) fn render_panel_header(
        &self,
        title: &str,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(SPACE_3))
            .gap(px(SPACE_2))
            .child(
                div()
                    .text_size(px(FONT_BODY))
                    .line_height(px(LEADING_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title.to_string()),
            )
    }

    fn render_history_panel(
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
        let mut panel = div().flex().flex_col().size_full().bg(theme.sidebar);
        let mut header = self.render_panel_header(title, theme);
        if !favorites_only {
            header = header.child(div().flex_1()).child(
                div()
                    .id("history-clear")
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
                    .w(px(28.))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
                    .child(icon(IconName::Trash, 14., theme.muted_foreground)),
            );
        }
        panel = panel.child(header);
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
            .overflow_y_scroll()
            .p(px(SPACE_2));
        let mut zebra = 0usize;
        for (ix, entry) in entries {
            let preview = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let sql = entry.sql.clone();
            let favorite = entry.favorite;
            zebra += 1;
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(px(RADIUS_MD))
                    .when(zebra % 2 == 1, |row| row.bg(theme.table_even))
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
                                    .text_size(px(FONT_LABEL))
                                    .line_height(px(LEADING_LABEL))
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
