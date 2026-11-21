use super::*;

use gpui_kit::component::{button::ButtonCustomVariant, FocusableExt as _};

/// Drag range of the expanded Connections sidebar.
pub(super) const SIDEBAR_MIN_WIDTH: f32 = 180.;
pub(super) const SIDEBAR_MAX_WIDTH: f32 = 480.;
/// Drag range of the explorer column next to it.
pub(super) const EXPLORER_MIN_WIDTH: f32 = 190.;
pub(super) const EXPLORER_MAX_WIDTH: f32 = 560.;

pub(super) fn clamp_sidebar_width(width: f32) -> f32 {
    width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
}

pub(super) fn clamp_explorer_width(width: f32) -> f32 {
    width.clamp(EXPLORER_MIN_WIDTH, EXPLORER_MAX_WIDTH)
}

/// Payload while the sidebar's right edge is dragged.
#[derive(Clone)]
pub(super) struct SidebarResize;

impl Render for SidebarResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

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
                self.sidebar_width
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
                    if this.rail == panel && panel == RailPanel::Connections {
                        this.leave_tools();
                    }
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
                                PopupMenuItem::new("Import connections…")
                                    .icon(Icon::new(IconName::Upload))
                                    .on_click(move |_, _, cx| {
                                        import_view
                                            .update(cx, |this, cx| this.import_connections(cx))
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Export Kilat connections…")
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
                nav = nav.child(self.render_tools_section(keyboard, theme, cx));
            }
        }
        nav = nav.when(!compact, |nav| {
            let edge = theme.primary.opacity(0.55);
            nav.relative()
                .on_drag_move(cx.listener(
                    |this, event: &gpui_kit::DragMoveEvent<SidebarResize>, _, cx| {
                        let width = clamp_sidebar_width(f32::from(
                            event.event.position.x - event.bounds.left(),
                        ));
                        if (width - this.sidebar_width).abs() >= 0.5 {
                            this.sidebar_width = width;
                            cx.notify();
                        }
                    },
                ))
                .child(
                    // A 6px grab strip on the right edge; a line shows on hover.
                    div()
                        .id("sidebar-resize-handle")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .hover(move |style| style.bg(edge))
                        .on_drag(SidebarResize, |drag, _, _, cx| {
                            cx.stop_propagation();
                            let drag = drag.clone();
                            cx.new(|_| drag)
                        })
                        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                            // Double click puts the default width back.
                            if event.click_count() == 2 {
                                this.sidebar_width = CONNECTIONS_WIDTH;
                                this.save_panel_layout();
                                cx.notify();
                            }
                        })),
                )
        });
        let settings = tree_button(Button::new("sidebar-settings"), keyboard, cx)
            .small()
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
            );
        let toggle = Button::new("toggle-sidebar")
            .ghost()
            .small()
            .icon(IconName::PanelLeft)
            .tooltip(if compact {
                "Expand connections sidebar"
            } else {
                "Collapse connections sidebar"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_compact = !this.sidebar_compact;
                cx.notify();
            }));
        // Expanded: Settings on the left, the toggle pulled to the right
        // edge. Compact rail (too narrow for both): toggle above Settings.
        nav.child(div().flex_1().min_h(px(0.))).child(if compact {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(toggle)
                .child(settings.w_full())
        } else {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_1))
                .child(settings.flex_1().min_w(px(0.)))
                .child(toggle)
        })
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
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                if event.click_count() == 2 {
                                    this.new_tab_with_sql(sql.clone(), window, cx);
                                    this.active_tab()
                                        .editor
                                        .read(cx)
                                        .focus_handle(cx)
                                        .focus(window, cx);
                                }
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
            self.push_message(format!("Failed to save query history: {err}"));
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
